"""Martian Code Review Bench adapter, offline route (github.com/withmartian/code-review-benchmark).

Reviews the 50 benchmark pull requests at their original base and head with the benchmark image
and adds the comments to Martian's `results/benchmark_data.json` as one more tool, so Martian's
own extraction, deduplication and judge steps score it next to the published tools:

    python3 bench/adapters/martian.py --martian-dir ../code-review-benchmark/offline \\
        --provider claude --authorize-paid-run 50
    cd ../code-review-benchmark/offline
    uv run python -m code_review_benchmark.step2_extract_comments --tool opencodereview-claude
    uv run python -m code_review_benchmark.step2_5_dedup_candidates --tool opencodereview-claude
    uv run python -m code_review_benchmark.step3_judge_comments --tool opencodereview-claude \\
        --dedup-groups results/<judge model>/dedup_groups.json
    uv run python -m code_review_benchmark.summary_table

Martian's leaderboard lists only tools that commented on GitHub (steps 0 and 1); see
bench/README.md for that route. Needs an authenticated `gh` for the GitHub pull request
metadata — Martian's dataset is still GitHub even though the product forge is GitLab.
"""

import argparse
import fcntl
import json
import os
import pathlib
import shutil
import subprocess
import sys
import threading

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import common  # noqa: E402


def pull_request(url):
    owner, name, _, number = url.rstrip("/").split("/")[-4:]
    # Martian URLs identify GitHub PRs; keep `gh` for this public benchmark only.
    raw = subprocess.run(
        ["gh", "api", f"repos/{owner}/{name}/pulls/{number}"], check=True, text=True, capture_output=True,
    ).stdout
    data = json.loads(raw)
    return f"{owner}/{name}", int(number), data["base"]["sha"], data["head"]["sha"], data["title"], data.get("body") or ""


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--martian-dir", required=True, type=pathlib.Path, help="code-review-benchmark/offline")
    parser.add_argument("--provider", default="claude")
    parser.add_argument("--tool", help="defaults to opencodereview-<provider>")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--image", default=os.environ.get("OCR_BENCH_IMAGE", "opencodereview-bench:local"))
    parser.add_argument("--authorize-paid-run", type=int, metavar="REVIEWS")
    parser.add_argument("--jobs", type=int, default=1, help="pull requests reviewed at a time")
    parser.add_argument("--shard", default="1/1", metavar="K/N",
                        help="review only the K-th of N slices, so N processes (one per account) split the set")
    args = parser.parse_args()
    common.check_provider(args.provider)

    tool = args.tool or f"opencodereview-{args.provider}"
    data_path = args.martian_dir / "results" / "benchmark_data.json"
    data = json.loads(data_path.read_text())
    shard, shards = (int(part) for part in args.shard.split("/"))
    if not 1 <= shard <= shards:
        raise SystemExit("--shard must be K/N with 1 <= K <= N")
    # Slices of the whole set, so a pull request stays in its slice when a rerun skips the done ones.
    urls = [
        url for index, url in enumerate(sorted(data))
        if index % shards == shard - 1 and not any(review["tool"] == tool for review in data[url].get("reviews") or [])
    ]
    urls = urls[: args.limit] if args.limit else urls
    run_dir = common.BENCH / "results" / "martian" / tool
    marker = "authorized.json" if shards == 1 else f"authorized-{shard}-of-{shards}.json"
    common.authorize(run_dir, len(urls), args.authorize_paid_run, args.provider, "Martian", marker)
    backup = data_path.with_suffix(".before-opencodereview.json")
    if not backup.exists():
        shutil.copyfile(data_path, backup)

    write_lock = threading.Lock()

    def one(index, url):
        common.log(f"== [{index}/{len(urls)}] {url}")
        try:
            nwo, number, base, head, title, body = pull_request(url)
            repo = common.checkout(nwo, f"https://github.com/{nwo}.git", base, head, number)
        except Exception as error:  # noqa: BLE001 - one bad pull request must not stop the run
            common.log(f"   {url}: skipped: {error}")
            return
        work = run_dir / "runs" / f"{nwo.replace('/', '__')}__{number}"
        try:
            status, findings, export = common.review(args.image, repo, nwo, number, base, head, title, body, work, args.provider)
        finally:
            common.release(repo)
        if status != 0 or findings is None:
            common.log(f"   {url}: failed; not recorded, so a later run retries it")
            return
        # A failed review is recorded as reviewed with no comments, as the benchmark scores it.
        review = {
            "tool": tool,
            "repo_name": f"{nwo.replace('/', '__')}__{tool}__PR{number}",
            "pr_url": url,
            "review_comments": [
                {"path": finding["file"], "line": finding["end_line"], "body": finding["message"]}
                for finding in findings["findings"]
            ],
            "ocr_status": (export or {}).get("status"),
        }
        # Re-read before each write: the file is the benchmark's working state, shared by the jobs
        # and by the other shards' processes.
        with write_lock, open(data_path.with_suffix(".lock"), "w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            data = json.loads(data_path.read_text())
            data[url].setdefault("reviews", [])
            data[url]["reviews"] = [item for item in data[url]["reviews"] or [] if item["tool"] != tool] + [review]
            temporary = data_path.with_suffix(".partial")
            temporary.write_text(json.dumps(data, indent=2, ensure_ascii=False))
            temporary.replace(data_path)

    common.run_all(urls, args.jobs, one)
    common.log(f"done: {tool} added to {data_path} (backup at {backup.name})")


if __name__ == "__main__":
    main()
