//! Typed input/output for ephemeral generic volume workers.
//!
//! Execution is owned by the async mesh (the same reason `talk` is): a worker
//! runs provider turns and tools, so it cannot be implemented by the sync tool
//! executor. Keeping the contract here still gives every provider one stable
//! native-tool schema and one validation boundary.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const MAX_VOLUME_TASK_CHARS: usize = 24_000;
pub const MAX_VOLUME_CONTEXT_CHARS: usize = 48_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeJobInput {
    pub id: String,
    pub task: String,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub expected_output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeWorkInput {
    pub objective: String,
    pub jobs: Vec<VolumeJobInput>,
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeJobResult {
    pub id: String,
    pub ok: bool,
    pub summary: String,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VolumeWorkResult {
    pub objective: String,
    pub requested: usize,
    pub completed: usize,
    pub failed: usize,
    pub results: Vec<VolumeJobResult>,
}

impl VolumeWorkInput {
    pub fn validate(&self, configured_limit: usize) -> Result<usize> {
        let objective = self.objective.trim();
        if objective.is_empty() || objective.chars().count() > 4_000 {
            bail!("volume objective must contain 1..=4000 characters");
        }
        if self.jobs.len() < 2 {
            bail!("volume_work needs at least two independent jobs; handle one item yourself or use a named coworker when judgment is needed");
        }
        let mut ids = std::collections::HashSet::new();
        for job in &self.jobs {
            let id = job.id.trim();
            if id.is_empty()
                || id.len() > 96
                || id.chars().any(char::is_control)
                || !ids.insert(id.to_string())
            {
                bail!("every volume job needs a unique, non-empty id up to 96 characters");
            }
            let task_len = job.task.chars().count();
            if task_len == 0 || task_len > MAX_VOLUME_TASK_CHARS {
                bail!("volume job `{id}` task must contain 1..={MAX_VOLUME_TASK_CHARS} characters");
            }
            if job
                .context
                .as_deref()
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or(0)
                > MAX_VOLUME_CONTEXT_CHARS
            {
                bail!("volume job `{id}` context exceeds {MAX_VOLUME_CONTEXT_CHARS} characters");
            }
        }
        // There is deliberately no product-wide worker ceiling. The caller's
        // setting is the default, while an individual batch may explicitly
        // choose any positive concurrency up to its own item count.
        Ok(self
            .max_concurrency
            .unwrap_or(configured_limit.max(1))
            .max(1)
            .min(self.jobs.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch() -> VolumeWorkInput {
        VolumeWorkInput {
            objective: "Classify the records".into(),
            jobs: vec![
                VolumeJobInput {
                    id: "a".into(),
                    task: "Classify A".into(),
                    context: None,
                    expected_output: None,
                },
                VolumeJobInput {
                    id: "b".into(),
                    task: "Classify B".into(),
                    context: None,
                    expected_output: None,
                },
            ],
            max_concurrency: Some(20),
        }
    }

    #[test]
    fn explicit_concurrency_is_only_bounded_by_the_batch_size() {
        assert_eq!(batch().validate(6).unwrap(), 2);
    }

    #[test]
    fn worker_batches_have_no_hard_job_or_concurrency_cap() {
        let mut value = batch();
        value.jobs = (0..96)
            .map(|index| VolumeJobInput {
                id: format!("item-{index}"),
                task: format!("Process item {index}"),
                context: None,
                expected_output: None,
            })
            .collect();
        value.max_concurrency = Some(96);
        assert_eq!(value.validate(8).unwrap(), 96);
    }

    #[test]
    fn one_job_is_not_a_volume_batch() {
        let mut value = batch();
        value.jobs.pop();
        assert!(value
            .validate(8)
            .unwrap_err()
            .to_string()
            .contains("at least two"));
    }

    #[test]
    fn batch_contract_has_no_per_item_wall_clock_limit() {
        let encoded = serde_json::to_value(batch()).unwrap();
        assert!(encoded.get("timeout_seconds").is_none());
    }
}
