"""Re-runs the gate/integrity job of a PR's current head when something it reads
changes without a pull_request_target event: a fixer-proof comment, a review/agent
verdict, or a finished pull_request workflow. It re-runs that one job, or the whole
run when no run had its other jobs succeed, and so updates the required context on the
head.

Usage: rerun_integrity.py  (reads GITHUB_EVENT_NAME and GITHUB_EVENT_PATH)
GATE_REPO, GATE_WORKFLOW (default gate-integrity.yml), GATE_EVENT (default
pull_request_target) and GATE_JOB (default gate/integrity) override the target.
"""
import json
import os
import subprocess
import sys
import time

WAIT_SECONDS, POLL_SECONDS = 20 * 60, 20


def event_target(name, event):
    """(PR number or None, the commit the event is about or None). A comment names its
    PR; a status and a workflow run name a commit (and a run its PRs)."""
    if name == 'issue_comment':
        return event['issue']['number'], None
    if name == 'status':
        return None, event['sha']
    run = event['workflow_run']
    return next((pr['number'] for pr in run.get('pull_requests') or []), None), run['head_sha']


def stale(event_sha, head):
    """True when the event is about a commit that is no longer the PR's head."""
    return event_sha is not None and event_sha != head


def resolve_pr(prs, sha):
    """prs: {number, state, head} for each PR that holds the commit (commits/{sha}/pulls).
    The open PR whose head is the commit, or None. In a stack every child PR holds its
    base PR's head too, so the first PR listed is not always the one."""
    return next((pr['number'] for pr in prs if pr['state'] == 'open' and pr['head'] == sha), None)


def pick(runs, jobs_by_run, job_name):
    """What to re-run once every run of the head has completed: ('job', job id, run id)
    in the newest run (by id) that has the job and whose other jobs all succeeded, else
    ('run', run id) for the newest run whole. Runs started in the same second (an open
    and a label) can be cancelled in either order, so the newest is not always usable."""
    for run in sorted(runs, key=lambda run: -run['id']):
        jobs = jobs_by_run[run['id']]
        job = next((j for j in jobs if j['name'] == job_name), None)
        if job and all(j['conclusion'] == 'success' for j in jobs if j is not job):
            return 'job', job['id'], run['id']
    return 'run', max(run['id'] for run in runs)


def gh(*args):
    return subprocess.run(['gh', *args], check=True, capture_output=True, text=True).stdout


def main():
    repo = os.environ.get('GATE_REPO', 'tylers2222/Monocode-Unlocked')
    workflow = os.environ.get('GATE_WORKFLOW', 'gate-integrity.yml')
    trigger = os.environ.get('GATE_EVENT', 'pull_request_target')
    job_name = os.environ.get('GATE_JOB', 'gate/integrity')
    with open(os.environ['GITHUB_EVENT_PATH']) as f:
        pr, event_sha = event_target(os.environ['GITHUB_EVENT_NAME'], json.load(f))
    if pr is None:
        pr = resolve_pr(json.loads(gh('api', f'repos/{repo}/commits/{event_sha}/pulls', '--jq', '[.[] | {number, state, head: .head.sha}]')), event_sha)
        if pr is None:
            print(f'rerun: {event_sha} is not the head of an open PR')
            return 0
    deadline = time.monotonic() + WAIT_SECONDS
    while True:
        head = gh('api', f'repos/{repo}/pulls/{pr}', '--jq', '.head.sha').strip()
        # A comment names no commit: pin the head it found first, so a push during the
        # wait does not re-run the new head (that push starts its own run).
        event_sha = event_sha or head
        if stale(event_sha, head):
            print(f'rerun: {event_sha} is not the head of #{pr} ({head}); nothing to re-run')
            return 0
        runs = json.loads(gh('api', f'repos/{repo}/actions/workflows/{workflow}/runs?head_sha={head}&event={trigger}&per_page=20',
                             '--jq', '[.workflow_runs[] | {id, status}]'))
        if not runs:
            print(f'rerun: no {workflow} run for {head}')
            return 0
        if all(run['status'] == 'completed' for run in runs):
            break
        if time.monotonic() > deadline:
            print(f'rerun: a {workflow} run for {head} is still running after {WAIT_SECONDS}s')
            return 1
        time.sleep(POLL_SECONDS)
    # The head may move while this waited; read it once more right before re-running.
    if gh('api', f'repos/{repo}/pulls/{pr}', '--jq', '.head.sha').strip() != head:
        print(f'rerun: #{pr} moved past {head} while waiting; nothing to re-run')
        return 0
    jobs_by_run = {run['id']: json.loads(gh('api', f'repos/{repo}/actions/runs/{run["id"]}/jobs',
                                            '--jq', '[.jobs[] | {id, name, conclusion}]')) for run in runs}
    choice = pick(runs, jobs_by_run, job_name)
    if choice[0] == 'job':
        gh('api', '-X', 'POST', f'repos/{repo}/actions/jobs/{choice[1]}/rerun')
        print(f'rerun: re-ran {job_name} (job {choice[1]}) of run {choice[2]} for {head}')
    else:
        gh('api', '-X', 'POST', f'repos/{repo}/actions/runs/{choice[1]}/rerun')
        print(f'rerun: re-ran run {choice[1]} whole for {head} (no run had its other jobs succeed)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
