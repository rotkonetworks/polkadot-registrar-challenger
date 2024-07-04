use crate::connector::DisplayNameEntry;
use crate::database::Database;
use crate::base::{ChainName, IdentityContext, JudgementState};
use crate::{DisplayNameConfig, Result};
use strsim::jaro;

const VIOLATIONS_CAP: usize = 5;

#[derive(Debug, Clone)]
pub struct DisplayNameVerifier {
    db: Database,
    config: DisplayNameConfig,
}

impl DisplayNameVerifier {
    pub fn new(db: Database, config: DisplayNameConfig) -> Self {
        DisplayNameVerifier { db, config }
    }

    pub async fn check_similarities(
        &self,
        name: &str,
        chain: ChainName,
        // Skip comparison for this account, usually for the issuer itself
        // (required when re-requesting judgement).
        skip: Option<&IdentityContext>,
    ) -> Result<Vec<DisplayNameEntry>> {
        let current = self.db.fetch_display_names(chain).await?;

        let mut violations = vec![];
        for existing in current {
            if let Some(to_skip) = skip {
                // Skip account if specified.
                if &existing.context == to_skip {
                    continue;
                }
            }

            if is_too_similar(name, &existing.display_name, self.config.limit) {
                // Only show up to `VIOLATIONS_CAP` violations.
                if violations.len() == VIOLATIONS_CAP {
                    break;
                }

                violations.push(existing);
            }
        }

        Ok(violations)
    }

    pub async fn verify_display_name(&self, state: &JudgementState) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        let name = if let Some(name) = state.display_name() {
            name
        } else {
            return Ok(());
        };

        let violations = self
            .check_similarities(name, state.context.chain, Some(&state.context))
            .await?;

        if !violations.is_empty() {
            self.db
                .insert_display_name_violations(&state.context, &violations)
                .await?;
        } else {
            self.db.set_display_name_valid(state).await?;
        }

        Ok(())
    }
}

fn is_too_similar(existing: &str, new: &str, limit: f64) -> bool {
    let name_str = existing.to_lowercase();
    let account_str = new.to_lowercase();

    let similarities = [
        jaro(&name_str, &account_str),
        jaro_words(&name_str, &account_str, &[" ", "-", "_"]),
    ];

    similarities.iter().any(|&s| s > limit)
}

fn jaro_words(left: &str, right: &str, delimiter: &[&str]) -> f64 {
    fn splitter<'a>(string: &'a str, delimiter: &[&str]) -> Vec<&'a str> {
        let mut all = vec![];

        for del in delimiter {
            let mut words: Vec<&str> = string
                .split(del)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();

            all.append(&mut words);
        }

        all
    }

    let left_words = splitter(left, delimiter);
    let right_words = splitter(right, delimiter);

    let mut total = 0.0;

    for left_word in &left_words {
        let mut temp = 0.0;

        for right_word in &right_words {
            let sim = jaro(left_word, right_word);

            if sim > temp {
                temp = sim;
            }
        }

        total += temp;
    }

    total as f64 / left_words.len().max(right_words.len()) as f64
}
