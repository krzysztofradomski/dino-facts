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
        let now = Self::now_min();
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
        let now = Self::now_min();
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
