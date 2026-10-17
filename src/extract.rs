use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct TriviaFact {
    pub dino: String,
    pub category: String,
    pub fact: String,
    pub wiki_url: String,
}

pub const MAX_FACTS_PER_DINO: usize = 8;
const MIN_SCORE: i32 = 6;

pub const SKIP_SECTIONS: &[&str] = &[
    "see also", "references", "external links", "further reading", "classification",
    "phylogeny", "notes", "bibliography", "gallery",
];
const GOOD_SECTIONS: &[&str] = &[
    "description", "paleobiology", "behavio", "feeding", "diet", "reproduction", "growth",
    "senses", "locomotion", "cultural", "etymology", "discovery", "paleoecology", "pathology",
];
const DRY_SECTIONS: &[&str] = &["history", "taxonomy", "species", "specimen", "systematics"];

// Scholarly hedging and taxonomy boilerplate: never trivia.
const REJECT_PHRASES: &[&str] = &[
    "is a genus of", "type species", "cladogram", "holotype", "paratype", "syntype", "et al",
    "suggested", "proposed", "argued", "hypothes", "disagree", "speculat", "nomen", "synonym",
    "reassign", "according to", "described by", "unknown", "uncertain", "debate", "a paper",
];
// Sentences that only make sense in the context of the previous one.
const DEPENDENT_STARTS: &[&str] = &[
    "however", "additionally", "furthermore", "moreover", "thus", "therefore", "also",
    "although", "while", "despite", "since", "because", "such", "these", "this", "those",
    "that", "they", "he", "she", "both", "another", "others", "then", "later", "but", "and",
    "or", "so", "yet", "instead", "likewise", "similarly", "conversely", "nevertheless",
    "consequently",
];
const TRIVIA_WORDS: &[&str] = &[
    "bite", "teeth", "tooth", "brain", "feather", "horn", "crest", "club", "armor", "armour",
    "spike", "sail", "claw", "vision", "smell", "hearing", "swim", "egg", "nest", "predator",
    "prey", "hunt", "swallow", "stomach", "fast", "speed", "weigh", "tall", "named", "means",
    "derived", "greek", "latin", "mistaken", "injur", "pathology", "herd", "sprint", "tail",
    "skin", "colour", "color", "jaw", "skull", "eat", "venom", "roar", "vocal", "footprint",
    "juvenile", "lifespan", "extinct",
];
const SUPERLATIVES: &[&str] = &[
    "largest", "smallest", "longest", "heaviest", "fastest", "tallest", "oldest", "strongest",
    "highest", "biggest", "first", "only", "most", "unique",
];
const UNITS: &[&str] = &[
    "m", "cm", "mm", "metres", "meters", "ft", "feet", "inches", "kg", "lb", "lbs", "pounds",
    "tonnes", "tons", "kilograms", "km/h", "mph", "kilometres", "kilometers", "million",
    "years",
];

fn is_year(t: &str) -> bool {
    t.len() == 4 && t.parse::<u16>().is_ok_and(|y| (1800..2100).contains(&y))
}

// Splits on ". " + uppercase, skipping initials ("J. Smith") and a few abbreviations.
fn split_sentences(line: &str) -> Vec<&str> {
    let b = line.as_bytes();
    let (mut out, mut start) = (Vec::new(), 0);
    for i in 0..b.len().saturating_sub(2) {
        if matches!(b[i], b'.' | b'!' | b'?') && b[i + 1] == b' ' && b[i + 2].is_ascii_uppercase() {
            let word = line[start..i].rsplit(' ').next().unwrap_or("");
            let initial = word.len() == 1 && word.chars().all(|c| c.is_uppercase());
            if initial || matches!(word, "St" | "Dr" | "Mr" | "Mt" | "sp" | "spp" | "cf" | "vs" | "No") {
                continue;
            }
            out.push(line[start..=i].trim());
            start = i + 2;
        }
    }
    out.push(line[start..].trim());
    out
}

// Specimen catalogue codes like "CM 9401", "NMMNH P-3698", "MOR1125".
fn has_specimen_code(toks: &[&str]) -> bool {
    toks.iter()
        .any(|t| t.chars().any(|c| c.is_ascii_uppercase()) && t.chars().any(|c| c.is_ascii_digit()))
        || toks.windows(2).any(|w| {
            let a = w[0].trim_matches(|c: char| !c.is_alphanumeric());
            a.len() >= 2 && a.chars().all(|c| c.is_ascii_uppercase()) && w[1].starts_with(|c: char| c.is_ascii_digit())
        })
}

fn has_measurement(toks: &[&str]) -> bool {
    toks.windows(2).any(|w| {
        w[0].chars().any(|c| c.is_ascii_digit())
            && UNITS.contains(&w[1].trim_matches(|c: char| !c.is_alphanumeric() && c != '/'))
    })
}

// None = reject outright; Some(score) = candidate, higher is better trivia.
fn score_sentence(s: &str, dino: &str, section: &str) -> Option<i32> {
    let n = s.len();
    if !(60..=200).contains(&n) || !s.starts_with(|c: char| c.is_uppercase()) || !s.ends_with('.') {
        return None;
    }
    let lower = s.to_lowercase();
    if REJECT_PHRASES.iter().any(|p| lower.contains(p)) {
        return None;
    }
    let first = lower.split(|c: char| !c.is_alphabetic()).next().unwrap_or("");
    if DEPENDENT_STARTS.contains(&first) {
        return None;
    }
    let toks: Vec<&str> = s.split_whitespace().collect();
    let cited_year = toks.iter().any(|t| {
        (t.starts_with('(') || t.ends_with(')')) && is_year(t.trim_matches(|c: char| !c.is_ascii_digit()))
    });
    if cited_year || has_specimen_code(&toks) {
        return None;
    }

    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let mut score = 2 * TRIVIA_WORDS.iter().filter(|k| words.iter().any(|w| w.starts_with(**k))).count().min(3) as i32;
    if SUPERLATIVES.iter().any(|k| words.contains(k)) {
        score += 3;
    }
    if has_measurement(&toks) {
        score += 3;
    } else if s.chars().any(|c| c.is_ascii_digit()) {
        score += 1;
    }
    let dino_l = dino.to_lowercase();
    if lower.contains(&dino_l) {
        score += 2;
    }
    if (80..=170).contains(&n) {
        score += 1;
    }
    if toks.iter().any(|t| is_year(t.trim_matches(|c: char| !c.is_ascii_digit()))) {
        score -= 2;
    }
    let names = toks
        .iter()
        .skip(1)
        .filter(|t| {
            t.starts_with(|c: char| c.is_uppercase())
                && !dino_l.contains(&t.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        })
        .count();
    if names >= 3 {
        score -= 3;
    }
    let sec = section.to_lowercase();
    if GOOD_SECTIONS.iter().any(|k| sec.contains(k)) {
        score += 1;
    }
    if DRY_SECTIONS.iter().any(|k| sec.contains(k)) {
        score -= 2;
    }
    (score >= MIN_SCORE).then_some(score)
}

pub fn extract_trivia_nuggets(dino_title: &str, full_text: &str) -> Vec<TriviaFact> {
    let wiki_url = format!("https://en.wikipedia.org/wiki/{}", dino_title.replace(' ', "_"));
    let (mut top, mut sub) = ("Overview".to_string(), String::new());
    let mut scored: Vec<(i32, TriviaFact)> = Vec::new();

    for raw_line in full_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("==") && line.ends_with("==") {
            let name = line.trim_matches('=').trim().to_string();
            if line.starts_with("===") {
                sub = name;
            } else {
                top = name;
                sub.clear();
            }
            continue;
        }
        let section = format!("{top} {sub}");
        if SKIP_SECTIONS.iter().any(|s| section.to_lowercase().contains(s)) {
            continue;
        }

        for sentence in split_sentences(line) {
            let mut s = sentence.to_string();
            if s.starts_with("It ") {
                s = format!("{}{}", dino_title, &s[2..]);
            } else if s.starts_with("Its ") {
                s = format!("{}'s {}", dino_title, &s[4..]);
            }
            let Some(score) = score_sentence(&s, dino_title, &section) else { continue };
            if scored.iter().any(|(_, f)| f.fact == s) {
                continue;
            }
            scored.push((
                score,
                TriviaFact {
                    dino: dino_title.to_string(),
                    category: top.clone(),
                    fact: s,
                    wiki_url: wiki_url.clone(),
                },
            ));
        }
    }
    // Stable sort: ties keep article order.
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.truncate(MAX_FACTS_PER_DINO);
    scored.into_iter().map(|(_, f)| f).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_trivia_drops_scholarly_noise() {
        let text = "Intro line.\n== Description ==\n\
            It could run at speeds of up to 40 kilometres per hour, outpacing most predators of its day. \
            However, the exact age and provenance of CM 9401 remain uncertain due to a lack of detailed field documentation. \
            Lehman and Carpenter (1990) described NMMNH P-3698 as belonging to a new tyrannosaurid genus, while others disagreed. \
            Extrapolating from this, Haas suggested that ankylosaurs ate relatively soft non-abrasive vegetation.\n\
            == References ==\n\
            It had enormous teeth that could crush bone with a force of 57,000 newtons or more.";
        let facts: Vec<String> = extract_trivia_nuggets("Tyrannosaurus", text).into_iter().map(|f| f.fact).collect();
        assert_eq!(facts.len(), 1, "{facts:?}");
        assert!(facts[0].starts_with("Tyrannosaurus could run"), "{facts:?}");
    }
}
