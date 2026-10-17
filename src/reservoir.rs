use crate::extract::TriviaFact;
use rand::{seq::SliceRandom, Rng};
use std::sync::RwLock;

pub const RESERVOIR_CAP: usize = 5_000;

/// The pool behind `GET /`: bounded, replaced per dinosaur, and rotating once full.
pub struct Reservoir {
    facts: RwLock<Vec<TriviaFact>>,
    cap: usize,
}

impl Reservoir {
    pub fn new(seed: Vec<TriviaFact>, cap: usize) -> Self {
        Self { facts: RwLock::new(seed), cap }
    }

    pub fn random(&self) -> Option<TriviaFact> {
        self.facts.read().unwrap().choose(&mut rand::thread_rng()).cloned()
    }

    // Swaps everything held for `dino` with `facts`. When full, random other facts are evicted so
    // newly harvested dinosaurs keep entering the pool instead of the pool freezing. Returns the new size.
    pub fn replace_dino(&self, dino: &str, facts: &[TriviaFact]) -> usize {
        let mut pool = self.facts.write().unwrap();
        pool.retain(|f| f.dino != dino);
        let fresh: Vec<&TriviaFact> = facts
            .iter()
            .filter(|f| !pool.iter().any(|e| e.fact == f.fact))
            .take(self.cap)
            .collect();
        let mut rng = rand::thread_rng();
        for _ in 0..(pool.len() + fresh.len()).saturating_sub(self.cap) {
            let i = rng.gen_range(0..pool.len());
            pool.swap_remove(i);
        }
        pool.extend(fresh.into_iter().cloned());
        pool.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Reservoir {
        fn all(&self) -> Vec<TriviaFact> {
            self.facts.read().unwrap().clone()
        }
    }

    fn fact(dino: &str, n: usize) -> TriviaFact {
        TriviaFact {
            dino: dino.into(),
            category: "c".into(),
            fact: format!("{dino} fact {n}"),
            wiki_url: String::new(),
        }
    }

    fn many(dino: &str, k: usize) -> Vec<TriviaFact> {
        (0..k).map(|n| fact(dino, n)).collect()
    }

    #[test]
    fn replacing_a_dino_drops_its_old_facts_and_keeps_others() {
        let r = Reservoir::new(many("A", 3), 100);
        r.replace_dino("B", &many("B", 2));
        r.replace_dino("A", &[fact("A", 99)]);
        let all = r.all();
        assert_eq!(all.len(), 3);
        assert!(all.iter().filter(|f| f.dino == "A").all(|f| f.fact == "A fact 99"));
        assert_eq!(all.iter().filter(|f| f.dino == "B").count(), 2);
    }

    #[test]
    fn when_full_new_facts_evict_random_old_ones() {
        let r = Reservoir::new(many("A", 10), 10);
        assert_eq!(r.replace_dino("B", &many("B", 3)), 10);
        let all = r.all();
        assert_eq!(all.iter().filter(|f| f.dino == "B").count(), 3);
        assert_eq!(all.iter().filter(|f| f.dino == "A").count(), 7);
    }

    #[test]
    fn identical_text_from_another_dino_is_not_added_twice() {
        let r = Reservoir::new(vec![fact("A", 1)], 10);
        let mut dup = fact("B", 1);
        dup.fact = "A fact 1".into();
        assert_eq!(r.replace_dino("B", &[dup]), 1);
    }

    #[test]
    fn random_is_none_when_empty() {
        assert!(Reservoir::new(vec![], 10).random().is_none());
    }
}
