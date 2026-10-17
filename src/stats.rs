use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

// --- 24h Zero-Allocation Rolling Telemetry (1440 x 1-minute slots) ---
struct MinuteSlot {
    epoch_min: AtomicU64,
    requests: AtomicU64,
    wiki_calls: AtomicU64,
}

pub struct Stats24h {
    slots: Vec<MinuteSlot>,
    total_facts_harvested: AtomicU64,
}

impl Stats24h {
    pub fn new() -> Self {
        Self {
            slots: (0..1440)
                .map(|_| MinuteSlot {
                    epoch_min: AtomicU64::new(0),
                    requests: AtomicU64::new(0),
                    wiki_calls: AtomicU64::new(0),
                })
                .collect(),
            total_facts_harvested: AtomicU64::new(1),
        }
    }

    fn now_min() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            / 60
    }

    fn current_slot(&self) -> &MinuteSlot {
        self.slot_at(Self::now_min())
    }

    fn slot_at(&self, now: u64) -> &MinuteSlot {
        let slot = &self.slots[(now % 1440) as usize];
        let prev = slot.epoch_min.load(Ordering::Relaxed);
        if prev != now
            && slot
                .epoch_min
                .compare_exchange(prev, now, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
        {
            slot.requests.store(0, Ordering::Relaxed);
            slot.wiki_calls.store(0, Ordering::Relaxed);
        }
        slot
    }

    pub fn rec_req(&self) {
        self.current_slot().requests.fetch_add(1, Ordering::Relaxed);
    }

    pub fn rec_wiki(&self) {
        self.current_slot().wiki_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub fn set_facts(&self, n: u64) {
        self.total_facts_harvested.store(n, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> (u64, u64, u64, Vec<u64>) {
        self.snapshot_at(Self::now_min())
    }

    fn snapshot_at(&self, now: u64) -> (u64, u64, u64, Vec<u64>) {
        let mut reqs = 0;
        let mut wiki = 0;
        let mut hourly = vec![0u64; 24];

        for i in 0..1440 {
            let m = now.saturating_sub(i);
            let s = &self.slots[(m % 1440) as usize];
            if s.epoch_min.load(Ordering::Relaxed) == m {
                let r = s.requests.load(Ordering::Relaxed);
                let w = s.wiki_calls.load(Ordering::Relaxed);
                reqs += r;
                wiki += w;
                hourly[23 - (i / 60) as usize] += r;
            }
        }
        (
            reqs,
            wiki,
            self.total_facts_harvested.load(Ordering::Relaxed),
            hourly,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_000_000;

    fn hit(s: &Stats24h, minute: u64) {
        s.slot_at(minute).requests.fetch_add(1, Ordering::Relaxed);
    }

    #[test]
    fn counts_requests_and_buckets_by_hour() {
        let s = Stats24h::new();
        hit(&s, T);
        hit(&s, T);
        hit(&s, T - 61);
        let (reqs, wiki, _, hourly) = s.snapshot_at(T);
        assert_eq!((reqs, wiki), (3, 0));
        assert_eq!(hourly[23], 2);
        assert_eq!(hourly[22], 1);
    }

    #[test]
    fn drops_data_older_than_24h() {
        let s = Stats24h::new();
        hit(&s, T);
        assert_eq!(s.snapshot_at(T + 1439).0, 1);
        assert_eq!(s.snapshot_at(T + 1440).0, 0);
    }

    #[test]
    fn reused_slot_is_reset() {
        let s = Stats24h::new();
        hit(&s, T);
        hit(&s, T);
        hit(&s, T + 1440);
        assert_eq!(s.snapshot_at(T + 1440).0, 1);
    }

    #[test]
    fn tracks_facts_in_reservoir() {
        let s = Stats24h::new();
        s.set_facts(42);
        assert_eq!(s.snapshot_at(T).2, 42);
    }
}
