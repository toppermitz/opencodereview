use super::*;
use crate::gitlab::StatusCommentApi;
use std::collections::BTreeMap;

const QUEUE_GENERATION_PREFIX: &str = "<!-- ocr-queue:v1 generation=";
const QUEUE_GENERATION_SUFFIX: &str = " -->";

fn queue_generation_marker(generation: i64) -> String {
    format!("{QUEUE_GENERATION_PREFIX}{generation}{QUEUE_GENERATION_SUFFIX}")
}

pub(crate) fn queue_acknowledgement_body(
    full_name: &str,
    number: i64,
    installation: &str,
    head: &str,
    generation: i64,
) -> String {
    format!(
        "{}\n{}\n\n{}",
        activation_marker(full_name, number, installation),
        queue_generation_marker(generation),
        queued_for_review_text(full_name, head)
    )
}

/// `Some(Some(generation))` for a generation-marked acknowledgement,
/// `Some(None)` for a legacy confirmation comment (marker followed by a blank
/// line), `None` for anything else.
pub(crate) fn owned_queue_comment_generation(body: &str, ownership_marker: &str) -> Option<Option<i64>> {
    let mut lines = body.lines();
    if lines.next()? != ownership_marker {
        return None;
    }
    let second = lines.next()?;
    if second.is_empty() {
        return Some(None);
    }
    second
        .strip_prefix(QUEUE_GENERATION_PREFIX)
        .and_then(|rest| rest.strip_suffix(QUEUE_GENERATION_SUFFIX))
        .and_then(|generation| generation.parse::<i64>().ok())
        .map(Some)
}

static QUEUE_ACKNOWLEDGEMENT_LOCK: Mutex<()> = Mutex::new(());

/// One process-wide pass posts each due acknowledgement and deletes each
/// retiring one; the lock prevents two passes from racing on the same
/// generation. Per-PR failures are recorded with backoff and aggregated, so
/// one PR's failure never stops the others.
pub(crate) fn reconcile_queue_acknowledgements_with(
    db: &Database,
    api: &mut impl StatusCommentApi,
) -> Result<(), String> {
    let _guard = QUEUE_ACKNOWLEDGEMENT_LOCK
        .lock()
        .map_err(|_| "queue acknowledgement lock poisoned".to_string())?;
    let installation_id = db.installation_id()?;
    let mut errors = Vec::new();
    for ack in db.due_queue_acknowledgements(crate::db::now_millis())? {
        let ownership = activation_marker(&ack.full_name, ack.number, &installation_id);
        let classifier = |body: &str| owned_queue_comment_generation(body, &ownership);
        let outcome = match ack.status.as_str() {
            "pending" => replace_owned_comment(
                api,
                &ack.full_name,
                ack.number,
                &classifier,
                ack.generation,
                &queue_acknowledgement_body(
                    &ack.full_name,
                    ack.number,
                    &installation_id,
                    &ack.head_sha,
                    ack.generation,
                ),
            )
            .map(Some),
            _ => delete_owned_comments_before(api, &ack.full_name, ack.number, &classifier, ack.generation + 1)
                .map(|_| None),
        };
        let recorded = outcome.as_ref().map(|id| *id).map_err(|error| error.as_str());
        if let Err(error) = db.record_queue_acknowledgement(ack.pr_id, ack.generation, &ack.status, recorded) {
            errors.push(format!("{} #{}: {error}", ack.full_name, ack.number));
        }
        if let Err(error) = outcome {
            errors.push(format!("{} #{}: {error}", ack.full_name, ack.number));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

pub(crate) fn reconcile_queue_acknowledgements(db: &Database) {
    let _ = reconcile_queue_acknowledgements_with(db, &mut gitlab::GitLabStatusComments);
}

/// Per-key background task: a request while a pass for the same key runs makes
/// that pass run once more instead of starting another thread.
pub(crate) struct CoalescedTask(Mutex<BTreeMap<String, bool>>);

impl CoalescedTask {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(BTreeMap::new()))
    }

    /// A present key means a pass is running and the value is the rerun flag.
    /// The running pass removes its key under the same lock acquisition as the
    /// rerun check so no request is lost; the drop guard frees the key if the
    /// task unwinds.
    pub(crate) fn spawn(&'static self, key: String, task: impl Fn() + Send + 'static) {
        {
            let mut running = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(rerun) = running.get_mut(&key) {
                *rerun = true;
                return;
            }
            running.insert(key.clone(), false);
        }
        thread::spawn(move || {
            struct Remove<'a> {
                task: &'a CoalescedTask,
                key: String,
            }
            impl Drop for Remove<'_> {
                fn drop(&mut self) {
                    if !thread::panicking() {
                        return;
                    }
                    self.task.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.key);
                }
            }
            let _remove = Remove {
                task: self,
                key: key.clone(),
            };
            loop {
                task();
                let mut running = self.0.lock().unwrap_or_else(|e| e.into_inner());
                match running.get_mut(&key) {
                    Some(rerun) if *rerun => *rerun = false,
                    _ => {
                        running.remove(&key);
                        return;
                    }
                }
            }
        });
    }
}

static QUEUE_ACKNOWLEDGEMENT_TASK: CoalescedTask = CoalescedTask::new();

/// The reconcile pass runs GitHub list/post/delete calls, so callers in request
/// or review-job paths hand it to a background pass with its own connection;
/// requests coalesce per database instead of starting a thread per call.
pub(crate) fn spawn_queue_acknowledgements(db_handle: Db) {
    QUEUE_ACKNOWLEDGEMENT_TASK.spawn(db_handle.id(), move || {
        // GitHub comments: one instance reconciles at a time. Waiting rather than skipping: a pass
        // already running elsewhere may have read the rows before this request's change.
        let _ = db_handle.run_exclusive_after(exclusive::QUEUE_ACKNOWLEDGEMENTS, || {
            if let Ok(db) = db_handle.open() {
                reconcile_queue_acknowledgements(&db);
            }
        });
    });
}

#[cfg(test)]
#[path = "queue_acknowledgements_tests.rs"]
mod tests;
