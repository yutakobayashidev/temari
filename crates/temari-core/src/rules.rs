use std::collections::HashSet;

use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ApprovedFolder, Classification, ClassificationBasis, Error, FileCandidate};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalRule {
    pub id: String,
    pub monitor_id: String,
    pub name_glob: String,
    pub destination_id: String,
    pub priority: i32,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuleMatch {
    pub rule_id: String,
    pub destination_id: String,
}

#[derive(Debug)]
pub struct RuleSet {
    rules: Vec<CompiledRule>,
    digest: String,
}

#[derive(Debug)]
struct CompiledRule {
    definition: LocalRule,
    matcher: GlobMatcher,
}

impl RuleSet {
    pub fn compile(rules: &[LocalRule], folders: &[ApprovedFolder]) -> Result<Self, Error> {
        let approved_ids: HashSet<_> = folders.iter().map(|folder| folder.id.as_str()).collect();
        let mut ids = HashSet::new();
        let mut enabled = Vec::new();
        for rule in rules {
            validate_rule(rule)?;
            if !ids.insert(rule.id.as_str()) {
                return Err(Error::InvalidState(format!(
                    "local rule IDs must be unique: {:?}",
                    rule.id
                )));
            }
            if !approved_ids.contains(rule.destination_id.as_str()) {
                return Err(Error::InvalidState(format!(
                    "local rule {:?} uses unknown approved destination ID {:?}",
                    rule.id, rule.destination_id
                )));
            }
            if !rule.enabled {
                continue;
            }
            let mut builder = GlobBuilder::new(&rule.name_glob);
            builder
                .case_insensitive(true)
                .literal_separator(true)
                .backslash_escape(false);
            let matcher = builder
                .build()
                .map_err(|error| {
                    Error::InvalidState(format!(
                        "invalid name glob for local rule {:?}: {error}",
                        rule.id
                    ))
                })?
                .compile_matcher();
            enabled.push(CompiledRule {
                definition: rule.clone(),
                matcher,
            });
        }
        enabled.sort_by(|left, right| {
            right
                .definition
                .priority
                .cmp(&left.definition.priority)
                .then_with(|| left.definition.id.cmp(&right.definition.id))
        });
        let canonical: Vec<_> = enabled.iter().map(|rule| &rule.definition).collect();
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&canonical).map_err(Error::Json)?)
        );
        Ok(Self {
            rules: enabled,
            digest,
        })
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn match_file(&self, file: &FileCandidate) -> Option<RuleMatch> {
        let basename = file.source_path.rsplit('/').next().unwrap_or_default();
        self.rules
            .iter()
            .find(|rule| rule.matcher.is_match(basename))
            .map(|rule| RuleMatch {
                rule_id: rule.definition.id.clone(),
                destination_id: rule.definition.destination_id.clone(),
            })
    }

    pub fn classify(&self, file: &FileCandidate) -> Option<Classification> {
        self.match_file(file).map(|matched| Classification {
            file_id: file.id.clone(),
            destination_id: matched.destination_id,
            reasoning: None,
            basis: ClassificationBasis::Rule,
            rule_id: Some(matched.rule_id),
        })
    }
}

fn validate_rule(rule: &LocalRule) -> Result<(), Error> {
    for (name, value) in [
        ("id", rule.id.as_str()),
        ("monitor_id", rule.monitor_id.as_str()),
        ("destination_id", rule.destination_id.as_str()),
    ] {
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err(Error::InvalidState(format!(
                "local rule {name} must be non-empty and contain no control characters"
            )));
        }
    }
    if rule.name_glob.trim().is_empty() || rule.name_glob.chars().any(char::is_control) {
        return Err(Error::InvalidState(
            "local rule name_glob must be non-empty and contain no control characters".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{FolderProposal, Proposal, ScanScope};

    use super::*;

    fn folders() -> Vec<ApprovedFolder> {
        Proposal {
            version: 2,
            source: "/tmp/inbox".into(),
            scope: ScanScope::default(),
            files_considered: 1,
            folders: vec![
                FolderProposal {
                    path: "Reports".into(),
                    description: "Reports".into(),
                },
                FolderProposal {
                    path: "Receipts".into(),
                    description: "Receipts".into(),
                },
            ],
        }
        .approve()
        .unwrap()
        .folders
    }

    fn rule(id: &str, pattern: &str, destination: &str, priority: i32) -> LocalRule {
        LocalRule {
            id: id.into(),
            monitor_id: "m1".into(),
            name_glob: pattern.into(),
            destination_id: destination.into(),
            priority,
            enabled: true,
        }
    }

    fn file(path: &str) -> FileCandidate {
        FileCandidate {
            id: "f1".into(),
            source_path: path.into(),
            extension: "pdf".into(),
        }
    }

    #[test]
    fn chooses_highest_priority_then_lowest_rule_id() {
        let rules = [
            rule("r2", "*.pdf", "d000002", 50),
            rule("r1", "*.pdf", "d000001", 50),
            rule("r3", "report*", "d000002", 40),
        ];
        let set = RuleSet::compile(&rules, &folders()).unwrap();

        assert_eq!(
            set.match_file(&file("nested/REPORT.pdf")).unwrap(),
            RuleMatch {
                rule_id: "r1".into(),
                destination_id: "d000001".into(),
            }
        );
    }

    #[test]
    fn matches_only_the_basename() {
        let rules = [rule("r1", "nested/*", "d000001", 50)];
        let set = RuleSet::compile(&rules, &folders()).unwrap();

        assert!(set.match_file(&file("nested/report.pdf")).is_none());
    }

    #[test]
    fn rejects_unknown_destinations_and_invalid_globs() {
        assert!(RuleSet::compile(&[rule("r1", "*.pdf", "unknown", 50)], &folders()).is_err());
        assert!(RuleSet::compile(&[rule("r1", "[", "d000001", 50)], &folders()).is_err());
    }

    #[test]
    fn digest_is_stable_but_changes_with_active_rules() {
        let first = rule("r1", "*.pdf", "d000001", 50);
        let second = rule("r2", "*.txt", "d000002", 40);
        let left = RuleSet::compile(&[first.clone(), second.clone()], &folders()).unwrap();
        let right = RuleSet::compile(&[second.clone(), first.clone()], &folders()).unwrap();
        assert_eq!(left.digest(), right.digest());

        let changed =
            RuleSet::compile(&[first, rule("r2", "*.md", "d000002", 40)], &folders()).unwrap();
        assert_ne!(left.digest(), changed.digest());
    }

    #[test]
    fn disabled_rules_do_not_match_or_affect_the_digest() {
        let active = rule("r1", "*.pdf", "d000001", 50);
        let mut disabled = rule("r2", "*.txt", "d000002", 40);
        disabled.enabled = false;
        let with_disabled = RuleSet::compile(&[active.clone(), disabled], &folders()).unwrap();
        let without_disabled = RuleSet::compile(&[active], &folders()).unwrap();

        assert_eq!(with_disabled.digest(), without_disabled.digest());
        assert!(with_disabled.match_file(&file("note.txt")).is_none());
    }
}
