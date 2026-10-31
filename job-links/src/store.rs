//! In-memory link store. All mutations go through one `RwLock`, so a claim
//! is atomic: two clients never get the same link.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::Serialize;

use crate::canon;
use crate::model::{
    ClaimRequest, LinkPatch, LinkRecord, ListQuery, NewLink, Platform, ResultRequest, Status,
};

pub const MAX_CLAIM: usize = 50;
pub const MAX_LIST: usize = 1000;
const MIN_LEASE_SECS: u64 = 30;
const MAX_LEASE_SECS: u64 = 24 * 3600;
const MAX_FIELD_CHARS: usize = 500;

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
    /// Insertion order = queue order.
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

fn fill(slot: &mut Option<String>, v: Option<String>) -> bool {
    match (slot.is_none(), clean(v)) {
        (true, Some(v)) => {
            *slot = Some(v);
            true
        }
        _ => false,
    }
}

fn set_or_clear(slot: &mut Option<String>, v: Option<String>) {
    if let Some(v) = v {
        *slot = clean(Some(v));
    }
}

fn release(l: &mut LinkRecord) {
    l.claimed_by = None;
    l.claimed_at = None;
    l.lease_expires_at = None;
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
                        l.status = Status::New;
                        l.status_reason = Some("lease expired".into());
                        release(l);
                        l.updated_at = now;
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

    /// Add a link, or merge metadata into the existing one for the same
    /// canonical URL and person. Never changes an existing link's status.
    pub fn upsert(&self, new: NewLink, now: DateTime<Utc>) -> Result<Upserted, StoreError> {
        let url = canon::canonicalize(&new.url).map_err(|e| StoreError::Invalid(e.to_string()))?;
        let person = clean(new.person_id);
        let key = dedupe_key(&url, person.as_deref());
        let mut st = self.write(now);
        if let Some(id) = st.by_key.get(&key).cloned() {
            let l = st.links.get_mut(&id).expect("index and links agree");
            let mut changed = false;
            changed |= fill(&mut l.title, new.title);
            changed |= fill(&mut l.company, new.company);
            changed |= fill(&mut l.location, new.location);
            changed |= fill(&mut l.mailbox, new.mailbox);
            changed |= fill(&mut l.email_id, new.email_id);
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
        let rec = LinkRecord {
            id: uuid::Uuid::new_v4().to_string(),
            original_url: new.url.trim().chars().take(4096).collect(),
            url,
            source: new.source.unwrap_or_default(),
            platform,
            title: clean(new.title),
            company: clean(new.company),
            location: clean(new.location),
            person_id: person,
            mailbox: clean(new.mailbox),
            email_id: clean(new.email_id),
            status: Status::New,
            status_reason: None,
            claimed_by: None,
            claimed_at: None,
            lease_expires_at: None,
            completed_at: None,
            created_at: now,
            updated_at: now,
            attempts: 0,
        };
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
        let mut st = self.write(now);
        let l = st.links.get_mut(id).ok_or(StoreError::NotFound)?;
        set_or_clear(&mut l.title, p.title);
        set_or_clear(&mut l.company, p.company);
        set_or_clear(&mut l.location, p.location);
        set_or_clear(&mut l.mailbox, p.mailbox);
        set_or_clear(&mut l.email_id, p.email_id);
        if let Some(s) = p.status {
            if s != l.status {
                release(l);
                l.completed_at = s.is_terminal().then_some(now);
                l.status = s;
            }
        }
        if p.status_reason.is_some() || p.status.is_some() {
            l.status_reason = clean(p.status_reason);
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

    /// Claim up to `limit` of the oldest `new` links for `client`.
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
        let mut st = self.write(now);
        let mut out = Vec::new();
        for l in st.links.values_mut() {
            if out.len() >= limit {
                break;
            }
            let wanted = l.status == Status::New
                && req.platforms.as_ref().is_none_or(|ps| ps.contains(&l.platform))
                && req
                    .person
                    .as_deref()
                    .is_none_or(|p| l.person_id.as_deref() == Some(p));
            if !wanted {
                continue;
            }
            l.status = Status::Claimed;
            l.status_reason = None;
            l.claimed_by = Some(client.clone());
            l.claimed_at = Some(now);
            l.lease_expires_at = Some(expires);
            l.attempts += 1;
            l.updated_at = now;
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
        l.status = req.status;
        l.status_reason = clean(req.reason);
        l.lease_expires_at = None;
        l.completed_at = Some(now);
        l.updated_at = now;
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
    fn dirty_tracking() {
        let s = Store::new();
        assert!(s.take_dirty().is_none());
        s.upsert(link("https://jobs.lever.co/acme/1", None), Utc::now()).unwrap();
        assert_eq!(s.take_dirty().unwrap().len(), 1);
        assert!(s.take_dirty().is_none());
    }
}
