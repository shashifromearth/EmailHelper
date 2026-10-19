//! Email workflow orchestration:
//! classify → (job / recruiter hand-off) → context → draft → validate → store
//! draft for Thunderbird → notify. Never sends email.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use crate::agents::{classifier, context, drafter, validator};
use crate::app::App;
use crate::db::{drafts, emails};
use crate::error::AppResult;
use crate::queue;
use crate::telegram::{format, Event};

pub async fn process_email(app: &Arc<App>, email_id: &str) -> AppResult<()> {
    let started = Instant::now();
    let id = email_id.to_string();
    let e = app.db.call(move |c| emails::get(c, &id)).await?;
    if e.status == "done" {
        return Ok(()); // already finished (retry after a crash between steps)
    }
    let id = e.id.clone();
    app.db
        .call(move |c| emails::set_status(c, &id, "processing"))
        .await?;

    // 1. Classification
    let cls = classifier::classify(&app.llm, &e).await;
    let cls_json = serde_json::to_string(&cls)?;
    let (id, cat, is_job) = (
        e.id.clone(),
        cls.category.as_str().to_string(),
        cls.is_job_opportunity,
    );
    app.db
        .call(move |c| emails::set_classification(c, &id, &cat, &cls_json, is_job))
        .await?;
    app.audit(
        "classifier",
        "email_classified",
        "email",
        &e.id,
        json!({ "category": cls.category.as_str(), "confidence": cls.confidence, "method": cls.method,
                "is_job": cls.is_job_opportunity, "needs_reply": cls.needs_reply,
                "injection_flags": cls.injection_flags }),
    )
    .await;

    // 2. Hand-offs to the job and recruiter agents (separate durable jobs).
    if cls.is_job_opportunity {
        if let Some(sig) = &cls.job_signal {
            app.metrics
                .jobs_detected_total
                .with_label_values(&[sig.source.as_str()])
                .inc();
        }
        if app.cfg.jobs.enabled {
            app.queue
                .enqueue(
                    queue::KIND_JOB_EMAIL,
                    format!("job_email:{}:v{}", e.id, app.cfg.workflow_version),
                    Some(e.id.clone()),
                    None,
                    app.cfg.jobs.max_attempts,
                )
                .await?;
        }
        if app.cfg.research.recruiter_enabled {
            app.queue
                .enqueue(
                    queue::KIND_RECRUITER,
                    format!("recruiter:{}:v{}", e.id, app.cfg.workflow_version),
                    Some(e.id.clone()),
                    None,
                    2,
                )
                .await?;
        }
        crate::jobs::track_recruiting_email(app, &e, &cls).await;
    }

    if !cls.needs_reply {
        finish(app, &e, &cls, "no_reply_needed", started).await?;
        return Ok(());
    }

    // 3. Context retrieval
    let ctx = match context::retrieve(app, &e).await {
        Ok(c) => c,
        Err(err) => {
            app.metrics
                .agent_failures_total
                .with_label_values(&["context"])
                .inc();
            return Err(err);
        }
    };

    // 4. Draft generation
    let draft = match drafter::generate(&app.llm, &e, &cls, &ctx).await {
        Ok(d) => d,
        Err(err) => {
            app.metrics
                .agent_failures_total
                .with_label_values(&["drafter"])
                .inc();
            return Err(err);
        }
    };

    // 5. Validation
    let v = validator::validate(
        &app.llm,
        &validator::ValidationInput {
            draft: &draft.body,
            grounding: &ctx.grounding,
            classification_confidence: cls.confidence,
            injection_flags: &cls.injection_flags,
        },
    )
    .await;
    if v.verdict == validator::Verdict::ReviewRequired {
        app.metrics.validation_failures_total.inc();
    }
    let mut notes = v.notes.clone();
    if !ctx.sources.is_empty() {
        notes.push(format!("Grounded in {}", ctx.sources.join(", ")));
    }

    // 6. Store draft for Thunderbird (the extension saves it; nothing is sent).
    let new = drafts::NewDraft {
        email_id: Some(e.id.clone()),
        kind: "reply".into(),
        account_id: e.account_id.clone(),
        tb_message_id: Some(e.tb_message_id),
        message_id: Some(e.message_id.clone()),
        to: vec![e.from_email.clone()],
        subject: draft.subject.clone(),
        body: draft.body.clone(),
        validation: v.verdict.as_str().into(),
        notes: notes.clone(),
        confidence: Some(v.confidence),
        llm_provider: Some(draft.provider.clone()),
        llm_model: Some(draft.model.clone()),
    };
    let draft_id = app.db.call(move |c| drafts::insert(c, &new)).await?;
    app.drafts_ready.notify_waiters();
    app.metrics
        .drafts_created_total
        .with_label_values(&[v.verdict.as_str()])
        .inc();
    app.audit(
        "drafter",
        "draft_created",
        "draft",
        &draft_id,
        json!({ "email_id": e.id, "validation": v.verdict.as_str(), "notes": v.notes,
                "provider": draft.provider, "model": draft.model, "sources": ctx.sources }),
    )
    .await;

    // 7. Notify
    let account = e
        .account_name
        .clone()
        .unwrap_or_else(|| e.account_id.clone());
    app.notifier
        .notify(
            Event::DraftCreated,
            &format::draft_created(&e.from, &e.subject, &account, &draft_id, v.verdict.as_str()),
        )
        .await;
    if v.verdict == validator::Verdict::ReviewRequired {
        app.notifier
            .notify(
                Event::ValidationFailed,
                &format::validation_failed(&e.from, &e.subject, &v.notes),
            )
            .await;
    }

    finish(app, &e, &cls, "draft_created", started).await
}

async fn finish(
    app: &Arc<App>,
    e: &emails::EmailRecord,
    cls: &classifier::Classification,
    outcome: &'static str,
    started: Instant,
) -> AppResult<()> {
    let id = e.id.clone();
    app.db
        .call(move |c| emails::set_status(c, &id, "done"))
        .await?;
    app.metrics
        .emails_processed_total
        .with_label_values(&[&e.account_type, cls.category.as_str()])
        .inc();
    let elapsed = started.elapsed();
    app.metrics
        .processing_duration_seconds
        .with_label_values(&["email"])
        .observe(elapsed.as_secs_f64());
    tracing::info!(
        email_id = %e.id,
        account_id = %e.account_id,
        category = cls.category.as_str(),
        outcome,
        duration_ms = elapsed.as_millis() as u64,
        "email processed"
    );
    app.audit(
        "pipeline",
        "email_processed",
        "email",
        &e.id,
        json!({ "outcome": outcome, "duration_ms": elapsed.as_millis() as u64 }),
    )
    .await;
    Ok(())
}
