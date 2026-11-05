use super::*;
use crate::review::ModelUsageEntry;

const NOW: i64 = 20_000 * DAY_MS + 12 * 3_600_000;

fn filter(days: i64) -> UsageFilter {
    UsageFilter {
        days,
        tz_minutes: 0,
        repo_id: None,
        now: NOW,
    }
}

struct Seed<'a> {
    db: &'a Database,
}

impl Seed<'_> {
    fn repo(&self, full_name: &str) -> i64 {
        let (owner, name) = full_name.split_once('/').expect("owner/name");
        self.db
            .conn
            .execute(
                "INSERT INTO repos (owner, name, full_name, is_private, created_at, updated_at) VALUES ($1, $2, $3, 0, 1, 1)",
                params![owner, name, full_name],
            )
            .expect("repo");
        self.db.conn.last_insert_rowid()
    }

    fn pr(&self, repo_id: i64, number: i64) -> i64 {
        self.db
            .conn
            .execute(
                "INSERT INTO pull_requests (repo_id, number, title, author, head_sha, last_synced_at, created_at, updated_at)
                 VALUES ($1, $2, 'Title', 'alice', 'head', 1, 1, 1)",
                params![repo_id, number],
            )
            .expect("pr");
        self.db.conn.last_insert_rowid()
    }

    fn run(
        &self,
        pr_id: i64,
        status: &str,
        created_at: i64,
        comments_json: Option<&str>,
        run_tokens: (i64, i64),
    ) -> i64 {
        self.db
            .conn
            .execute(
                "INSERT INTO review_runs (pr_id, head_sha, status, comments_json, created_at, updated_at, started_at,
                   input_tokens, output_tokens, model)
                 VALUES ($1, 'abc123', $2, $3, $4, $4, $4, $5, $6, 'claude')",
                params![pr_id, status, comments_json, created_at, run_tokens.0, run_tokens.1],
            )
            .expect("run");
        self.db.conn.last_insert_rowid()
    }

    #[allow(clippy::too_many_arguments)]
    fn stage(
        &self,
        run_id: i64,
        pr_id: i64,
        stage: &str,
        chunk_id: Option<i64>,
        cache_hit: bool,
        tokens: (i64, i64, i64, i64),
        at: (i64, i64),
    ) {
        self.db
            .conn
            .execute(
                "INSERT INTO review_run_stages (run_id, pr_id, stage, chunk_id, status, cache_hit, input_tokens, output_tokens,
                   cache_read_tokens, cache_creation_tokens, created_at, finished_at)
                 VALUES ($1, $2, $3, $4, 'ok', $5, $6, $7, $8, $9, $10, $11)",
                params![
                    run_id,
                    pr_id,
                    stage,
                    chunk_id,
                    i64::from(cache_hit),
                    tokens.0,
                    tokens.1,
                    tokens.2,
                    tokens.3,
                    at.0,
                    at.1
                ],
            )
            .expect("stage");
    }

    fn account(&self, id: i64, provider: &str, label: &str) -> AiAccount {
        AiAccount {
            id,
            provider: provider.into(),
            label: label.into(),
            priority: id,
            enabled: true,
            status: "ok".into(),
            limited_until: None,
            last_error: None,
            last_used_at: None,
            created_at: 1,
        }
    }
}

fn entry(model: &str, tokens: (i64, i64, i64, i64)) -> ModelUsageEntry {
    ModelUsageEntry {
        model: model.into(),
        input_tokens: tokens.0,
        output_tokens: tokens.1,
        cache_read_tokens: tokens.2,
        cache_creation_tokens: tokens.3,
    }
}

fn sum_tokens(rows: &[UsageGroupRow]) -> i64 {
    rows.iter().map(|row| row.usage.tokens).sum()
}

/// Two PRs in one repository: a posted run attributed to two accounts, a run superseded before it
/// was published, a legacy run without stages or attempts, a failed run that lost its usage and a
/// run older than the window.
#[test]
fn usage_report_totals_reconcile_across_every_breakdown() {
    let db = Db::for_test().open().expect("open db");
    let seed = Seed { db: &db };
    let repo = seed.repo("acme/api");
    let (pr1, pr2) = (seed.pr(repo, 1), seed.pr(repo, 2));
    let day = |days_ago: i64| NOW - days_ago * DAY_MS;

    let posted = seed.run(
        pr1,
        "posted",
        day(3),
        Some(r#"{"commit_id":"abc","comments":[{"body":"**[Achado] A**"},{"body":"**[Pergunta] B**"}]}"#),
        (999, 999),
    );
    seed.stage(
        posted,
        pr1,
        "chunk",
        Some(1),
        false,
        (100, 10, 50, 5),
        (day(3), day(3) + 60_000),
    );
    seed.stage(
        posted,
        pr1,
        "chunk",
        Some(2),
        true,
        (0, 0, 0, 0),
        (day(3), day(3) + 1_000),
    );
    seed.stage(
        posted,
        pr1,
        "final",
        None,
        false,
        (20, 2, 0, 0),
        (day(3) + 60_000, day(3) + 90_000),
    );
    db.insert_usage_attempts(
        posted,
        pr1,
        "chunks",
        &seed.account(1, "claude", "Primary"),
        &[entry("claude-opus", (100, 10, 50, 5))],
    )
    .expect("attempt");
    db.insert_usage_attempts(
        posted,
        pr1,
        "final",
        &seed.account(2, "codex", "Backup"),
        &[entry("codex", (20, 2, 0, 0))],
    )
    .expect("attempt");

    let superseded = seed.run(
        pr1,
        "superseded",
        day(2),
        Some(r#"{"comments":[{"body":"[Achado] C"}]}"#),
        (0, 0),
    );
    seed.stage(
        superseded,
        pr1,
        "single",
        None,
        false,
        (200, 20, 0, 0),
        (day(2), day(2) + 30_000),
    );
    // Its audit kept a finding and a question (kind read from the body); discarded, unmapped and
    // inherited candidates do not count.
    db.conn
        .execute(
            "INSERT INTO review_finding_audits (run_id, audit_json, updated_at) VALUES ($1, $2, 1)",
            params![
                superseded,
                r#"{"complete":true,"candidates":[
                  {"kind":"finding","origin":"chunk","aiDecision":"kept","body":"[Achado] C"},
                  {"kind":"","origin":"final","aiDecision":"kept","body":"**[Pergunta] E**"},
                  {"kind":"finding","origin":"chunk","aiDecision":"discarded","body":"[Achado] F"},
                  {"kind":"finding","origin":"inherited","aiDecision":"kept","body":"[Achado] G"},
                  {"kind":"finding","origin":"final","aiDecision":"unmapped","body":"[Achado] H"}]}"#
            ],
        )
        .expect("audit");

    let legacy = seed.run(pr2, "approved", day(1), Some(r#"[{"body":"[Achado] D"}]"#), (30, 3));
    db.set_run_model_usage(legacy, pr2, &[("claude-sonnet".into(), 30, 3, 0, 0)])
        .expect("model usage");

    let lost = seed.run(pr2, "failed", day(1), Some("not json"), (0, 0));
    seed.stage(lost, pr2, "single", None, false, (0, 0, 0, 0), (day(1), day(1) + 5_000));
    // A seeded audit from before the final pass decided anything counts nothing as found.
    db.conn
        .execute(
            "INSERT INTO review_finding_audits (run_id, audit_json, updated_at) VALUES ($1, $2, 1)",
            params![
                lost,
                r#"{"complete":false,"candidates":[{"kind":"finding","origin":"chunk","aiDecision":"pending","body":"[Achado] P"}]}"#
            ],
        )
        .expect("seeded audit");

    seed.run(pr2, "posted", day(40), None, (5_000, 500));

    let report = db.usage_report(&filter(30)).expect("report");
    let totals = &report.totals;
    assert_eq!(totals.usage.tokens, 137 + 220 + 33);
    assert_eq!(totals.usage.cache_read_tokens, 50);
    assert_eq!((totals.runs, totals.prs), (4, 2));
    // Only published runs count, and the superseded run never was.
    assert_eq!((totals.findings, totals.questions), (2, 1));
    // Found counts every run's kept findings, published or not.
    assert_eq!((totals.findings_found, totals.questions_found), (3, 2));
    assert_eq!(totals.overhead_tokens, 220);
    assert_eq!(totals.runs_without_usage, 1);
    assert_eq!((totals.reused_chunks, totals.chunks), (1, 2));
    // Durations: 90s, 30s and 5s; the legacy run has no stages.
    assert_eq!(totals.median_duration_ms, Some(30_000));

    assert_eq!(report.by_day.len(), 30);
    assert_eq!(
        report.by_day.iter().map(|day| day.tokens).sum::<i64>(),
        totals.usage.tokens
    );
    assert_eq!(report.by_day.iter().map(|day| day.runs).sum::<i64>(), 4);
    assert_eq!(sum_tokens(&report.by_repo), totals.usage.tokens);
    assert_eq!(report.by_repo[0].findings, Some(2));
    assert_eq!(report.by_repo[0].findings_found, Some(3));

    for breakdown in [&report.by_model, &report.by_account, &report.by_provider] {
        assert_eq!(sum_tokens(breakdown), totals.usage.tokens);
        assert_eq!(breakdown.last().map(|row| row.key.as_str()), Some("unattributed"));
    }
    let tokens_of =
        |rows: &[UsageGroupRow], key: &str| rows.iter().find(|row| row.key == key).map(|row| row.usage.tokens);
    assert_eq!(tokens_of(&report.by_account, "1"), Some(115));
    assert_eq!(tokens_of(&report.by_account, "2"), Some(22));
    assert_eq!(tokens_of(&report.by_account, "unattributed"), Some(220 + 33));
    assert_eq!(tokens_of(&report.by_model, "claude-sonnet"), Some(33));
    assert_eq!(tokens_of(&report.by_model, "unattributed"), Some(220));
    assert_eq!(tokens_of(&report.by_provider, "claude"), Some(115 + 33));
    assert_eq!(tokens_of(&report.by_provider, "codex"), Some(22));
    assert_eq!(report.by_account[0].label, "Primary");
    assert_eq!(report.by_account[0].provider.as_deref(), Some("claude"));

    assert_eq!(
        report.repos,
        vec![UsageRepoOption {
            id: repo,
            full_name: "acme/api".into()
        }]
    );
    assert!(!report.has_running);

    let page = db.usage_prs(&filter(30), 0, 1).expect("prs");
    assert_eq!((page.total, page.next_offset), (2, Some(1)));
    let top = &page.items[0];
    assert_eq!((top.pr_id, top.usage.tokens, top.runs, top.findings), (pr1, 357, 2, 1));
    assert_eq!(top.findings_found, 2);
    assert_eq!(
        top.strip
            .iter()
            .map(|run| (run.run_id, run.outcome))
            .collect::<Vec<_>>(),
        vec![(posted, "posted"), (superseded, "superseded")]
    );
    let rest = db.usage_prs(&filter(30), 1, 1).expect("prs");
    assert_eq!((rest.items[0].pr_id, rest.next_offset), (pr2, None));

    let runs = db.pr_usage_runs(pr1, &filter(30)).expect("runs");
    assert_eq!(
        runs.iter().map(|run| run.id).collect::<Vec<_>>(),
        vec![superseded, posted]
    );
    assert_eq!(runs[1].accounts, vec!["Backup".to_string(), "Primary".to_string()]);
    assert_eq!(runs[1].models, vec!["claude-opus".to_string(), "codex".to_string()]);
    assert_eq!(
        (runs[1].findings, runs[1].questions, runs[1].duration_ms),
        (1, 1, Some(90_000))
    );
    assert_eq!((runs[1].reused_chunks, runs[1].chunks), (1, 2));
    assert!(runs[0].accounts.is_empty());
    assert_eq!(
        (runs[0].findings, runs[0].findings_found, runs[0].questions_found),
        (0, 1, 1)
    );
    let lost_run = db.pr_usage_runs(pr2, &filter(30)).expect("runs");
    assert!(
        !lost_run
            .iter()
            .find(|run| run.id == lost)
            .expect("lost run")
            .usage_reported
    );
    assert!(
        lost_run
            .iter()
            .find(|run| run.id == legacy)
            .expect("legacy run")
            .usage_reported
    );
}

#[test]
fn usage_report_filters_by_repository_and_lists_every_repository() {
    let db = Db::for_test().open().expect("open db");
    let seed = Seed { db: &db };
    let (api, web) = (seed.repo("acme/api"), seed.repo("acme/web"));
    seed.run(seed.pr(api, 1), "posted", NOW - 1_000, None, (10, 1));
    seed.run(seed.pr(web, 1), "running", NOW - 1_000, None, (20, 2));

    let report = db
        .usage_report(&UsageFilter {
            repo_id: Some(web),
            ..filter(7)
        })
        .expect("report");
    assert_eq!(report.totals.usage.tokens, 22);
    assert!(report.has_running);
    assert_eq!(report.repos.len(), 2);
    assert_eq!(report.by_day.len(), 7);
}

#[test]
fn usage_days_are_the_viewers_calendar_days() {
    let db = Db::for_test().open().expect("open db");
    let seed = Seed { db: &db };
    let pr = seed.pr(seed.repo("acme/api"), 1);
    let today_utc = 20_000 * DAY_MS;
    // 23:30 UTC yesterday is already today one hour east of UTC.
    seed.run(pr, "posted", today_utc - 30 * 60_000, None, (10, 0));
    // 7 days back at 00:30 local (23:30 UTC the day before) is the window's first moment.
    seed.run(
        pr,
        "posted",
        today_utc - 7 * DAY_MS + 30 * 60_000 - 60 * 60_000 + DAY_MS,
        None,
        (5, 0),
    );
    // One minute before that local midnight is outside the window.
    seed.run(
        pr,
        "posted",
        today_utc - 6 * DAY_MS - 60 * 60_000 - 60_000,
        None,
        (1, 0),
    );

    let east = UsageFilter {
        tz_minutes: 60,
        ..filter(7)
    };
    let report = db.usage_report(&east).expect("report");
    assert_eq!(report.totals.usage.tokens, 15);
    assert_eq!(
        report.by_day.first().map(|day| (day.day.as_str(), day.tokens)),
        Some(("2024-09-28", 5))
    );
    assert_eq!(
        report.by_day.last().map(|day| (day.day.as_str(), day.tokens)),
        Some(("2024-10-04", 10))
    );
}

#[test]
fn run_outcomes_and_calendar_dates() {
    assert_eq!(run_outcome("queued", false), "running");
    assert_eq!(run_outcome("resolved", false), "posted");
    assert_eq!(run_outcome("approved", false), "posted");
    assert_eq!(run_outcome("preview", false), "preview");
    assert_eq!(run_outcome("preview", true), "posted");
    assert_eq!(run_outcome("superseded", true), "superseded");
    assert_eq!(civil_date(0), "1970-01-01");
    assert_eq!(civil_date(19_723), "2024-01-01");
    assert_eq!(civil_date(20_000), "2024-10-04");
    assert_eq!(median(vec![4, 1, 3, 2]), Some(2));
    assert_eq!(median(Vec::new()), None);
}
