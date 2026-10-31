//! In-memory link store. All mutations go through one `RwLock`, so a claim
//! is atomic: two clients never get the same link.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::Serialize;

use crate::canon;
use crate::model::{
    ClaimRequest, LinkPatch, LinkRecord, ListQuery, NewLink, Platform, Remote, ResultRequest,
    Status, StatusChange,
};

pub const MAX_CLAIM: usize = 50;
pub const MAX_LIST: usize = 1000;
const MIN_LEASE_SECS: u64 = 30;
const MAX_LEASE_SECS: u64 = 24 * 3600;
const MAX_FIELD_CHARS: usize = 500;
const MAX_DESCRIPTION_BYTES: usize = 4096;
const MAX_SKILLS: usize = 50;
const MAX_SKILL_CHARS: usize = 100;
const MAX_HISTORY: usize = 50;
/// Upper bound for a claim's `max_age_hours` (10 years), so the cutoff never overflows.
const MAX_AGE_HOURS: u64 = 10 * 365 * 24;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("link not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Invalid(String),
}

#[derive(Default)]
struct State {
    /// Insertion order = discovery order.
    links: IndexMap<String, LinkRecord>,
    /// Dedupe key → id.
    by_key: HashMap<String, String>,
    /// Earliest lease expiry, so reaping is a no-op until then.
    next_expiry: Option<DateTime<Utc>>,
}

#[derive(Default)]
pub struct Store {
    state: RwLock<State>,
    dirty: AtomicBool,
}

/// Result of an upsert.
#[derive(Debug, Clone, Serialize)]
pub struct Upserted {
    pub created: bool,
    pub link: LinkRecord,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub total: usize,
    pub by_status: BTreeMap<&'static str, usize>,
    pub by_platform: BTreeMap<&'static str, usize>,
}

fn dedupe_key(url: &str, person: Option<&str>) -> String {
    format!("{}\u{1f}{url}", person.unwrap_or(""))
}

/// Trimmed, length-capped, `None` when empty.
fn clean(v: Option<String>) -> Option<String> {
    let v = v?;
    let t = v.trim();
    (!t.is_empty()).then(|| t.chars().take(MAX_FIELD_CHARS).collect())
}

/// `full-time` / `Full Time` → `full_time`.
fn clean_kind(v: Option<String>) -> Option<String> {
    clean(v).map(|k| k.to_lowercase().replace(['-', ' '], "_"))
}

/// Trimmed and cut to about 4 KB on a character boundary.
fn clean_description(v: Option<String>) -> Option<String> {
    let v = v?;
    let t = v.trim();
    if t.is_empty() {
        return None;
    }
    let mut end = t.len().min(MAX_DESCRIPTION_BYTES);
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    Some(t[..end].to_string())
}

/// Trimmed, de-duplicated (case-insensitively) and capped.
fn clean_skills(skills: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in skills {
        let s: String = s.trim().chars().take(MAX_SKILL_CHARS).collect();
        if !s.is_empty() && !out.iter().any(|x| x.eq_ignore_ascii_case(&s)) && out.len() < MAX_SKILLS {
            out.push(s);
        }
    }
    out
}

fn check_score(score: Option<u8>) -> Result<Option<u8>, StoreError> {
    match score {
        Some(s) if s > 100 => Err(StoreError::Invalid("score must be 0-100".into())),
        s => Ok(s),
    }
}

/// Fill an empty field; true when it changed.
fn fill<T>(slot: &mut Option<T>, v: Option<T>) -> bool {
    if slot.is_none() && v.is_some() {
        *slot = v;
        true
    } else {
        false
    }
}

fn set_or_clear(slot: &mut Option<String>, v: Option<String>, cleaner: fn(Option<String>) -> Option<String>) {
    if let Some(v) = v {
        *slot = cleaner(Some(v));
    }
}

fn release(l: &mut LinkRecord) {
    l.claimed_by = None;
    l.claimed_at = None;
    l.lease_expires_at = None;
}

/// Change status, recording it in the history.
fn set_status(l: &mut LinkRecord, status: Status, reason: Option<String>, now: DateTime<Utc>) {
    l.status = status;
    l.status_reason = reason.clone();
    l.updated_at = now;
    l.status_history.push(StatusChange {
        status,
        reason,
        at: now,
    });
    if l.status_history.len() > MAX_HISTORY {
        let extra = l.status_history.len() - MAX_HISTORY;
        l.status_history.drain(..extra);
    }
}

/// When the job appeared: its posting date, else when it was first seen.
fn freshness(l: &LinkRecord) -> DateTime<Utc> {
    l.posted_at.unwrap_or(l.discovered_at)
}

type ClaimKey = (Reverse<i16>, bool, Reverse<DateTime<Utc>>, usize);

/// Claim order: higher score first (unscored last), then remote or
/// contract-type work, then the newest posting (first seen when undated),
/// then discovery order.
fn priority(l: &LinkRecord, index: usize) -> ClaimKey {
    let flexible = l.remote == Remote::Remote
        || l.employment_type
            .as_deref()
            .is_some_and(|k| ["contract", "freelance", "consultant", "consulting"].contains(&k));
    (Reverse(l.score.map_or(-1, i16::from)), !flexible, Reverse(freshness(l)), index)
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild from a snapshot (later duplicates of a key are dropped).
    pub fn from_records(records: Vec<LinkRecord>) -> Self {
        let mut st = State::default();
        for r in records {
            let key = dedupe_key(&r.url, r.person_id.as_deref());
            if st.by_key.contains_key(&key) || st.links.contains_key(&r.id) {
                continue;
            }
            st.by_key.insert(key, r.id.clone());
            st.links.insert(r.id.clone(), r);
        }
        st.next_expiry = st.links.values().filter_map(|l| l.lease_expires_at).min();
        Self {
            state: RwLock::new(st),
            dirty: AtomicBool::new(false),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Write lock with expired leases already returned to `new`.
    fn write(&self, now: DateTime<Utc>) -> std::sync::RwLockWriteGuard<'_, State> {
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        if st.next_expiry.is_some_and(|t| t <= now) {
            let mut next = None;
            let mut reaped = 0usize;
            for l in st.links.values_mut() {
                match l.lease_expires_at {
                    Some(t) if l.status == Status::Claimed && t <= now => {
                        tracing::info!(id = %l.id, client = ?l.claimed_by, "lease expired; link back to new");
                        set_status(l, Status::New, Some("lease expired".into()), now);
                        release(l);
                        reaped += 1;
                    }
                    Some(t) if l.status == Status::Claimed => {
                        next = Some(next.map_or(t, |n: DateTime<Utc>| n.min(t)));
                    }
                    _ => {}
                }
            }
            st.next_expiry = next;
            if reaped > 0 {
                self.mark_dirty();
            }
        }
        st
    }

    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::SeqCst);
    }

    /// All records when something changed since the last call.
    pub fn take_dirty(&self) -> Option<Vec<LinkRecord>> {
        self.dirty
            .swap(false, Ordering::SeqCst)
            .then(|| self.records())
    }

    pub fn records(&self) -> Vec<LinkRecord> {
        self.read().links.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.read().links.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Add a link, or fill empty fields of the existing one for the same
    /// canonical URL and person. Never overwrites a value or changes status.
    pub fn upsert(&self, new: NewLink, now: DateTime<Utc>) -> Result<Upserted, StoreError> {
        let url = canon::canonicalize(&new.url).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let score = check_score(new.score)?;
        let person = clean(new.person_id);
        let key = dedupe_key(&url, person.as_deref());
        let mut st = self.write(now);
        if let Some(id) = st.by_key.get(&key).cloned() {
            let l = st.links.get_mut(&id).expect("index and links agree");
            let mut changed = false;
            changed |= fill(&mut l.source_detail, clean(new.source_detail));
            changed |= fill(&mut l.title, clean(new.title));
            changed |= fill(&mut l.company, clean(new.company));
            changed |= fill(&mut l.location, clean(new.location));
            changed |= fill(&mut l.employment_type, clean_kind(new.employment_type));
            changed |= fill(&mut l.salary, clean(new.salary));
            changed |= fill(&mut l.description, clean_description(new.description));
            changed |= fill(&mut l.score, score);
            changed |= fill(&mut l.decision, clean_kind(new.decision));
            changed |= fill(&mut l.resume_hint, clean(new.resume_hint));
            changed |= fill(&mut l.mailbox, clean(new.mailbox));
            changed |= fill(&mut l.posted_at, new.posted_at);
            if l.remote == Remote::Unknown {
                if let Some(r) = new.remote.filter(|r| *r != Remote::Unknown) {
                    l.remote = r;
                    changed = true;
                }
            }
            if l.skills.is_empty() {
                l.skills = clean_skills(new.skills);
                changed |= !l.skills.is_empty();
            }
            if changed {
                l.updated_at = now;
                self.mark_dirty();
            }
            return Ok(Upserted {
                created: false,
                link: l.clone(),
            });
        }
        let detected = canon::classify(&url);
        let platform = match (detected, new.platform) {
            (Platform::Other | Platform::CareerSite, Some(given)) => given,
            (d, _) => d,
        };
        let mut rec = LinkRecord {
            id: uuid::Uuid::new_v4().to_string(),
            original_url: new.url.trim().chars().take(4096).collect(),
            url,
            source: new.source.unwrap_or_default(),
            source_detail: clean(new.source_detail),
            platform,
            title: clean(new.title),
            company: clean(new.company),
            location: clean(new.location),
            remote: new.remote.unwrap_or_default(),
            employment_type: clean_kind(new.employment_type),
            salary: clean(new.salary),
            skills: clean_skills(new.skills),
            description: clean_description(new.description),
            score,
            decision: clean_kind(new.decision),
            person_id: person,
            resume_hint: clean(new.resume_hint),
            mailbox: clean(new.mailbox),
            posted_at: new.posted_at,
            status: Status::New,
            status_reason: None,
            status_history: Vec::new(),
            claimed_by: None,
            claimed_at: None,
            lease_expires_at: None,
            completed_at: None,
            discovered_at: now,
            updated_at: now,
            attempts: 0,
        };
        set_status(&mut rec, Status::New, None, now);
        st.by_key.insert(key, rec.id.clone());
        st.links.insert(rec.id.clone(), rec.clone());
        self.mark_dirty();
        Ok(Upserted {
            created: true,
            link: rec,
        })
    }

    /// `(total matching, page)`.
    pub fn list(&self, q: &ListQuery, now: DateTime<Utc>) -> (usize, Vec<LinkRecord>) {
        drop(self.write(now));
        let st = self.read();
        let matches = |l: &&LinkRecord| {
            q.status.is_none_or(|s| l.status == s)
                && q.platform.is_none_or(|p| l.platform == p)
                && q.source.is_none_or(|s| l.source == s)
                && q.since.is_none_or(|t| l.updated_at >= t)
                && q.person
                    .as_deref()
                    .is_none_or(|p| l.person_id.as_deref() == Some(p))
        };
        let limit = q.limit.unwrap_or(100).min(MAX_LIST);
        let offset = q.offset.unwrap_or(0);
        let asc = q.order.as_deref().is_some_and(|o| o.eq_ignore_ascii_case("asc"));
        let all: Box<dyn Iterator<Item = &LinkRecord>> = if asc {
            Box::new(st.links.values())
        } else {
            Box::new(st.links.values().rev())
        };
        let filtered: Vec<&LinkRecord> = all.filter(matches).collect();
        let page = filtered
            .iter()
            .skip(offset)
            .take(limit)
            .map(|l| (*l).clone())
            .collect();
        (filtered.len(), page)
    }

    pub fn get(&self, id: &str, now: DateTime<Utc>) -> Result<LinkRecord, StoreError> {
        self.write(now)
            .links
            .get(id)
            .cloned()
            .ok_or(StoreError::NotFound)
    }

    /// Manual edit. Status may be set to anything except `claimed` (use a
    /// claim for that); setting `new` releases any claim.
    pub fn patch(&self, id: &str, p: LinkPatch, now: DateTime<Utc>) -> Result<LinkRecord, StoreError> {
        if p.status == Some(Status::Claimed) {
            return Err(StoreError::Invalid("use POST /v1/links/claim to claim links".into()));
        }
        let score = check_score(p.score)?;
        let mut st = self.write(now);
        let l = st.links.get_mut(id).ok_or(StoreError::NotFound)?;
        set_or_clear(&mut l.title, p.title, clean);
        set_or_clear(&mut l.company, p.company, clean);
        set_or_clear(&mut l.location, p.location, clean);
        set_or_clear(&mut l.employment_type, p.employment_type, clean_kind);
        set_or_clear(&mut l.salary, p.salary, clean);
        set_or_clear(&mut l.description, p.description, clean_description);
        set_or_clear(&mut l.decision, p.decision, clean_kind);
        set_or_clear(&mut l.resume_hint, p.resume_hint, clean);
        set_or_clear(&mut l.mailbox, p.mailbox, clean);
        if let Some(r) = p.remote {
            l.remote = r;
        }
        if score.is_some() {
            l.score = score;
        }
        match p.status {
            Some(s) if s != l.status => {
                release(l);
                l.completed_at = s.is_terminal().then_some(now);
                set_status(l, s, clean(p.status_reason), now);
            }
            _ if p.status_reason.is_some() => l.status_reason = clean(p.status_reason),
            _ => {}
        }
        l.updated_at = now;
        let out = l.clone();
        self.mark_dirty();
        Ok(out)
    }

    pub fn delete(&self, id: &str, now: DateTime<Utc>) -> Result<LinkRecord, StoreError> {
        let mut st = self.write(now);
        let l = st.links.shift_remove(id).ok_or(StoreError::NotFound)?;
        st.by_key
            .remove(&dedupe_key(&l.url, l.person_id.as_deref()));
        self.mark_dirty();
        Ok(l)
    }

    /// Claim up to `limit` `new` links for `client`, highest priority first
    /// (see [`priority`]). With `max_age_hours`, links posted (or first
    /// seen) longer ago are skipped but kept.
    pub fn claim(
        &self,
        req: ClaimRequest,
        default_lease: Duration,
        now: DateTime<Utc>,
    ) -> Result<(Vec<LinkRecord>, DateTime<Utc>), StoreError> {
        let client = req.client.trim().to_string();
        if client.is_empty() {
            return Err(StoreError::Invalid("client is required".into()));
        }
        let limit = req.limit.unwrap_or(1).clamp(1, MAX_CLAIM);
        let lease = req
            .lease_secs
            .unwrap_or(default_lease.as_secs())
            .clamp(MIN_LEASE_SECS, MAX_LEASE_SECS);
        let expires = now + chrono::Duration::seconds(lease as i64);
        let oldest = req
            .max_age_hours
            .filter(|h| *h > 0)
            .map(|h| now - chrono::Duration::hours(h.min(MAX_AGE_HOURS) as i64));
        let mut st = self.write(now);
        let mut candidates: Vec<ClaimKey> = st
            .links
            .values()
            .enumerate()
            .filter(|(_, l)| {
                l.status == Status::New
                    && oldest.is_none_or(|t| freshness(l) >= t)
                    && req.platforms.as_ref().is_none_or(|ps| ps.contains(&l.platform))
                    && req
                        .person
                        .as_deref()
                        .is_none_or(|p| l.person_id.as_deref() == Some(p))
            })
            .map(|(i, l)| priority(l, i))
            .collect();
        candidates.sort_unstable();
        let mut out = Vec::new();
        for (_, _, _, i) in candidates.into_iter().take(limit) {
            let (_, l) = st.links.get_index_mut(i).expect("index from enumerate");
            set_status(l, Status::Claimed, Some(format!("claimed by {client}")), now);
            l.claimed_by = Some(client.clone());
            l.claimed_at = Some(now);
            l.lease_expires_at = Some(expires);
            l.attempts += 1;
            out.push(l.clone());
        }
        if !out.is_empty() {
            st.next_expiry = Some(st.next_expiry.map_or(expires, |n| n.min(expires)));
            self.mark_dirty();
        }
        Ok((out, expires))
    }

    /// Record the outcome of an application attempt.
    pub fn result(&self, id: &str, req: ResultRequest, now: DateTime<Utc>) -> Result<LinkRecord, StoreError> {
        if !req.status.is_terminal() {
            return Err(StoreError::Invalid("status must be applied, failed or skipped".into()));
        }
        let mut st = self.write(now);
        let l = st.links.get_mut(id).ok_or(StoreError::NotFound)?;
        if l.status.is_terminal() {
            if l.status == req.status {
                return Ok(l.clone());
            }
            return Err(StoreError::Conflict(format!(
                "link is already {}",
                l.status.as_str()
            )));
        }
        if let (Status::Claimed, Some(holder), Some(client)) =
            (l.status, l.claimed_by.as_deref(), req.client.as_deref())
        {
            if holder != client.trim() {
                return Err(StoreError::Conflict(format!("link is claimed by {holder}")));
            }
        }
        set_status(l, req.status, clean(req.reason), now);
        l.lease_expires_at = None;
        l.completed_at = Some(now);
        let out = l.clone();
        self.mark_dirty();
        Ok(out)
    }

    pub fn stats(&self, now: DateTime<Utc>) -> Stats {
        drop(self.write(now));
        let st = self.read();
        let mut by_status: BTreeMap<&'static str, usize> =
            Status::ALL.iter().map(|s| (s.as_str(), 0)).collect();
        let mut by_platform = BTreeMap::new();
        for l in st.links.values() {
            *by_status.entry(l.status.as_str()).or_default() += 1;
            *by_platform.entry(l.platform.as_str()).or_default() += 1;
        }
        Stats {
            total: st.links.len(),
            by_status,
            by_platform,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(url: &str, person: Option<&str>) -> NewLink {
        NewLink {
            url: url.into(),
            person_id: person.map(Into::into),
            ..Default::default()
        }
    }

    fn claim(client: &str, limit: usize) -> ClaimRequest {
        ClaimRequest {
            client: client.into(),
            limit: Some(limit),
            ..Default::default()
        }
    }

    const LEASE: Duration = Duration::from_secs(600);

    #[test]
    fn dedupes_on_canonical_url_and_person() {
        let s = Store::new();
        let now = Utc::now();
        let a = s
            .upsert(link("https://jobs.lever.co/acme/1?utm_source=x", Some("p1")), now)
            .unwrap();
        assert!(a.created);
        assert_eq!(a.link.url, "https://jobs.lever.co/acme/1");
        assert_eq!(a.link.platform, Platform::Lever);
        let b = s
            .upsert(
                NewLink {
                    title: Some("Staff Engineer".into()),
                    ..link("https://jobs.lever.co/acme/1#apply", Some("p1"))
                },
                now,
            )
            .unwrap();
        assert!(!b.created);
        assert_eq!(b.link.id, a.link.id);
        assert_eq!(b.link.title.as_deref(), Some("Staff Engineer"));
        // Another person gets their own record.
        assert!(s.upsert(link("https://jobs.lever.co/acme/1", Some("p2")), now).unwrap().created);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn upsert_never_resets_status_or_overwrites_metadata() {
        let s = Store::new();
        let now = Utc::now();
        let id = s
            .upsert(
                NewLink {
                    title: Some("Original".into()),
                    ..link("https://jobs.lever.co/acme/1", None)
                },
                now,
            )
            .unwrap()
            .link
            .id;
        s.claim(claim("ext", 1), LEASE, now).unwrap();
        s.result(
            &id,
            ResultRequest {
                status: Status::Applied,
                reason: None,
                client: Some("ext".into()),
            },
            now,
        )
        .unwrap();
        let again = s
            .upsert(
                NewLink {
                    title: Some("Changed".into()),
                    ..link("https://jobs.lever.co/acme/1", None)
                },
                now,
            )
            .unwrap();
        assert_eq!(again.link.status, Status::Applied);
        assert_eq!(again.link.title.as_deref(), Some("Original"));
    }

    #[test]
    fn claims_are_exclusive_and_ordered() {
        let s = Store::new();
        let now = Utc::now();
        for i in 0..5 {
            s.upsert(link(&format!("https://jobs.lever.co/acme/{i}"), None), now)
                .unwrap();
        }
        let (a, _) = s.claim(claim("a", 2), LEASE, now).unwrap();
        let (b, _) = s.claim(claim("b", 10), LEASE, now).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 3);
        assert!(a[0].url.ends_with("/0") && a[1].url.ends_with("/1"));
        assert!(a.iter().all(|l| !b.iter().any(|x| x.id == l.id)));
        assert!(s.claim(claim("c", 1), LEASE, now).unwrap().0.is_empty());
        assert_eq!(a[0].attempts, 1);
        assert!(s.claim(claim(" ", 1), LEASE, now).is_err());
    }

    #[test]
    fn claim_filters_by_platform_and_person() {
        let s = Store::new();
        let now = Utc::now();
        s.upsert(link("https://www.linkedin.com/jobs/view/1/", Some("p1")), now).unwrap();
        s.upsert(link("https://jobs.lever.co/acme/1", Some("p2")), now).unwrap();
        let (got, _) = s
            .claim(
                ClaimRequest {
                    platforms: Some(vec![Platform::Lever]),
                    ..claim("x", 5)
                },
                LEASE,
                now,
            )
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].platform, Platform::Lever);
        let (got, _) = s
            .claim(
                ClaimRequest {
                    person: Some("p2".into()),
                    ..claim("x", 5)
                },
                LEASE,
                now,
            )
            .unwrap();
        assert!(got.is_empty(), "p2's only link is already claimed");
    }

    #[test]
    fn expired_lease_returns_link_to_new() {
        let s = Store::new();
        let now = Utc::now();
        let id = s.upsert(link("https://jobs.lever.co/acme/1", None), now).unwrap().link.id;
        let req = ClaimRequest {
            lease_secs: Some(60),
            ..claim("a", 1)
        };
        s.claim(req, LEASE, now).unwrap();
        assert_eq!(s.get(&id, now).unwrap().status, Status::Claimed);
        let later = now + chrono::Duration::seconds(61);
        let l = s.get(&id, later).unwrap();
        assert_eq!(l.status, Status::New);
        assert_eq!(l.claimed_by, None);
        assert_eq!(l.status_reason.as_deref(), Some("lease expired"));
        let (again, _) = s.claim(claim("b", 1), LEASE, later).unwrap();
        assert_eq!(again[0].attempts, 2);
        assert_eq!(again[0].claimed_by.as_deref(), Some("b"));
    }

    #[test]
    fn result_rules() {
        let s = Store::new();
        let now = Utc::now();
        let id = s.upsert(link("https://jobs.lever.co/acme/1", None), now).unwrap().link.id;
        let res = |status, client: Option<&str>| ResultRequest {
            status,
            reason: Some("r".into()),
            client: client.map(Into::into),
        };
        assert!(matches!(s.result(&id, res(Status::New, None), now), Err(StoreError::Invalid(_))));
        s.claim(claim("a", 1), LEASE, now).unwrap();
        assert!(matches!(
            s.result(&id, res(Status::Applied, Some("b")), now),
            Err(StoreError::Conflict(_))
        ));
        let done = s.result(&id, res(Status::Failed, Some("a")), now).unwrap();
        assert_eq!(done.status, Status::Failed);
        assert_eq!(done.lease_expires_at, None);
        assert!(done.completed_at.is_some());
        // Idempotent for the same status, a conflict for another.
        assert!(s.result(&id, res(Status::Failed, None), now).is_ok());
        assert!(matches!(
            s.result(&id, res(Status::Applied, None), now),
            Err(StoreError::Conflict(_))
        ));
        assert_eq!(s.result("nope", res(Status::Applied, None), now), Err(StoreError::NotFound));
    }

    #[test]
    fn patch_and_delete() {
        let s = Store::new();
        let now = Utc::now();
        let id = s.upsert(link("https://jobs.lever.co/acme/1", None), now).unwrap().link.id;
        s.claim(claim("a", 1), LEASE, now).unwrap();
        let p = s
            .patch(
                &id,
                LinkPatch {
                    title: Some("Lead".into()),
                    status: Some(Status::New),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(p.title.as_deref(), Some("Lead"));
        assert_eq!(p.status, Status::New);
        assert_eq!(p.claimed_by, None);
        let p = s
            .patch(
                &id,
                LinkPatch {
                    title: Some(String::new()),
                    ..Default::default()
                },
                now,
            )
            .unwrap();
        assert_eq!(p.title, None);
        assert!(s
            .patch(
                &id,
                LinkPatch {
                    status: Some(Status::Claimed),
                    ..Default::default()
                },
                now
            )
            .is_err());
        s.delete(&id, now).unwrap();
        assert_eq!(s.get(&id, now), Err(StoreError::NotFound));
        // The dedupe key is free again.
        assert!(s.upsert(link("https://jobs.lever.co/acme/1", None), now).unwrap().created);
    }

    #[test]
    fn list_filters_and_pages() {
        let s = Store::new();
        let now = Utc::now();
        for i in 0..4 {
            s.upsert(link(&format!("https://jobs.lever.co/acme/{i}"), Some("p1")), now)
                .unwrap();
        }
        s.upsert(link("https://www.naukri.com/job-listings-x-9", Some("p2")), now)
            .unwrap();
        let q = ListQuery {
            platform: Some(Platform::Lever),
            limit: Some(2),
            offset: Some(1),
            order: Some("asc".into()),
            ..Default::default()
        };
        let (total, page) = s.list(&q, now);
        assert_eq!(total, 4);
        assert_eq!(page.len(), 2);
        assert!(page[0].url.ends_with("/1"));
        let (total, page) = s.list(&ListQuery::default(), now);
        assert_eq!(total, 5);
        assert_eq!(page[0].platform, Platform::Naukri, "newest first by default");
        let stats = s.stats(now);
        assert_eq!(stats.by_status["new"], 5);
        assert_eq!(stats.by_platform["lever"], 4);
    }

    #[test]
    fn claim_order_is_score_then_flexible() {
        let s = Store::new();
        let now = Utc::now();
        let add = |n: u32, score: Option<u8>, remote: Remote, kind: Option<&str>| {
            s.upsert(
                NewLink {
                    score,
                    remote: Some(remote),
                    employment_type: kind.map(Into::into),
                    ..link(&format!("https://jobs.lever.co/acme/{n}"), None)
                },
                now,
            )
            .unwrap();
        };
        add(0, None, Remote::Remote, None);
        add(1, Some(70), Remote::Onsite, None);
        add(2, Some(70), Remote::Onsite, Some("Contract"));
        add(3, Some(90), Remote::Onsite, None);
        add(4, Some(70), Remote::Remote, None);
        let (got, _) = s.claim(claim("x", 10), LEASE, now).unwrap();
        let order: Vec<&str> = got.iter().map(|l| l.url.rsplit('/').next().unwrap()).collect();
        assert_eq!(order, ["3", "2", "4", "1", "0"]);
        assert_eq!(got[1].employment_type.as_deref(), Some("contract"));
    }

    #[test]
    fn ties_go_to_the_newest_posting() {
        let s = Store::new();
        let t0 = Utc::now();
        let add = |n: u32, score: u8, posted: Option<DateTime<Utc>>, seen: DateTime<Utc>| {
            s.upsert(
                NewLink {
                    score: Some(score),
                    posted_at: posted,
                    ..link(&format!("https://jobs.lever.co/acme/{n}"), None)
                },
                seen,
            )
            .unwrap();
        };
        add(0, 70, Some(t0 - chrono::Duration::hours(48)), t0);
        add(1, 70, Some(t0 - chrono::Duration::hours(1)), t0);
        // undated: first seen counts, and it was seen after 1 was posted
        add(2, 70, None, t0 + chrono::Duration::seconds(10));
        add(3, 90, Some(t0 - chrono::Duration::hours(100)), t0);
        let (got, _) = s.claim(claim("x", 10), LEASE, t0 + chrono::Duration::seconds(20)).unwrap();
        let order: Vec<&str> = got.iter().map(|l| l.url.rsplit('/').next().unwrap()).collect();
        assert_eq!(order, ["3", "2", "1", "0"], "score first, then newest");
    }

    #[test]
    fn max_age_skips_old_links_without_deleting_them() {
        let s = Store::new();
        let now = Utc::now();
        let old = now - chrono::Duration::days(20);
        s.upsert(NewLink { posted_at: Some(old), ..link("https://jobs.lever.co/acme/posted-long-ago", None) }, now)
            .unwrap();
        s.upsert(link("https://jobs.lever.co/acme/seen-long-ago", None), old).unwrap();
        s.upsert(
            NewLink {
                posted_at: Some(now - chrono::Duration::hours(2)),
                ..link("https://jobs.lever.co/acme/fresh", None)
            },
            old,
        )
        .unwrap();
        let req = |h: Option<u64>| ClaimRequest { max_age_hours: h, ..claim("x", 10) };
        let (got, _) = s.claim(req(Some(336)), LEASE, now).unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].url.ends_with("/fresh"), "a recent posting date wins over an old first-seen time");
        assert_eq!(s.len(), 3, "old links are kept");
        assert_eq!(s.stats(now).by_status["new"], 2);
        // 0 (or no limit at all) claims them too
        let (got, _) = s.claim(req(Some(0)), LEASE, now).unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn status_history_records_every_change() {
        let s = Store::new();
        let now = Utc::now();
        let id = s.upsert(link("https://jobs.lever.co/acme/1", None), now).unwrap().link.id;
        s.claim(ClaimRequest { lease_secs: Some(60), ..claim("a", 1) }, LEASE, now).unwrap();
        let later = now + chrono::Duration::seconds(61);
        s.claim(claim("b", 1), LEASE, later).unwrap();
        let l = s
            .result(
                &id,
                ResultRequest {
                    status: Status::Skipped,
                    reason: Some("needs visa".into()),
                    client: Some("b".into()),
                },
                later,
            )
            .unwrap();
        let h: Vec<(Status, Option<&str>)> =
            l.status_history.iter().map(|c| (c.status, c.reason.as_deref())).collect();
        assert_eq!(
            h,
            [
                (Status::New, None),
                (Status::Claimed, Some("claimed by a")),
                (Status::New, Some("lease expired")),
                (Status::Claimed, Some("claimed by b")),
                (Status::Skipped, Some("needs visa")),
            ]
        );
    }

    #[test]
    fn job_fields_are_cleaned_and_merged() {
        let s = Store::new();
        let now = Utc::now();
        let first = s
            .upsert(
                NewLink {
                    source: Some(crate::model::Source::Research),
                    source_detail: Some("remotive".into()),
                    skills: vec!["Rust".into(), " rust ".into(), "".into(), "Python".into()],
                    description: Some("é".repeat(3000)),
                    score: Some(88),
                    decision: Some("Apply".into()),
                    ..link("https://jobs.lever.co/acme/1", Some("p1"))
                },
                now,
            )
            .unwrap()
            .link;
        assert_eq!(first.skills, ["Rust", "Python"]);
        assert!(first.description.as_ref().unwrap().len() <= 4096);
        assert_eq!(first.decision.as_deref(), Some("apply"));
        let merged = s
            .upsert(
                NewLink {
                    source_detail: Some("adzuna".into()),
                    score: Some(10),
                    remote: Some(Remote::Hybrid),
                    resume_hint: Some("python".into()),
                    skills: vec!["Go".into()],
                    ..link("https://jobs.lever.co/acme/1", Some("p1"))
                },
                now,
            )
            .unwrap()
            .link;
        assert_eq!(merged.source_detail.as_deref(), Some("remotive"), "never overwritten");
        assert_eq!(merged.score, Some(88));
        assert_eq!(merged.skills, ["Rust", "Python"]);
        assert_eq!(merged.remote, Remote::Hybrid, "unknown is filled");
        assert_eq!(merged.resume_hint.as_deref(), Some("python"));
        assert!(s
            .upsert(
                NewLink {
                    score: Some(101),
                    ..link("https://jobs.lever.co/acme/2", None)
                },
                now
            )
            .is_err());
    }

    #[test]
    fn list_since_filters_on_updated_at() {
        let s = Store::new();
        let t0 = Utc::now();
        s.upsert(link("https://jobs.lever.co/acme/1", None), t0).unwrap();
        let t1 = t0 + chrono::Duration::seconds(10);
        s.upsert(link("https://jobs.lever.co/acme/2", None), t1).unwrap();
        let q = ListQuery {
            since: Some(t1),
            ..Default::default()
        };
        let (total, page) = s.list(&q, t1);
        assert_eq!(total, 1);
        assert!(page[0].url.ends_with("/2"));
    }

    #[test]
    fn dirty_tracking() {
        let s = Store::new();
        assert!(s.take_dirty().is_none());
        s.upsert(link("https://jobs.lever.co/acme/1", None), Utc::now()).unwrap();
        assert_eq!(s.take_dirty().unwrap().len(), 1);
        assert!(s.take_dirty().is_none());
    }
}
