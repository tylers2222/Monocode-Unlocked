"""Re-runs the gate/integrity job of a PR head when something it reads changes
without a pull_request_target event: a fixer-proof comment, a review/agent verdict,
or a finished CI or gate workflow. A re-run of that one job keeps the run's
gate/tier and gate/mutation results and updates the required context on the head.

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


def head_of(name, event):
    """('pr', number) or ('sha', sha) for the PR head the event concerns. The workflow's
    `if:` lets through only the events gate/integrity reads."""
    if name == 'issue_comment':
        return 'pr', event['issue']['number']
    if name == 'status':
        return 'sha', event['sha']
    return 'sha', event['workflow_run']['head_sha']


def gh(*args):
    return subprocess.run(['gh', *args], check=True, capture_output=True, text=True).stdout


def main():
    repo = os.environ.get('GATE_REPO', 'tylers2222/Monocode-Unlocked')
    workflow = os.environ.get('GATE_WORKFLOW', 'gate-integrity.yml')
    trigger = os.environ.get('GATE_EVENT', 'pull_request_target')
    job_name = os.environ.get('GATE_JOB', 'gate/integrity')
    with open(os.environ['GITHUB_EVENT_PATH']) as f:
        kind, value = head_of(os.environ['GITHUB_EVENT_NAME'], json.load(f))
    sha = gh('api', f'repos/{repo}/pulls/{value}', '--jq', '.head.sha').strip() if kind == 'pr' else value
    deadline = time.monotonic() + WAIT_SECONDS
    while True:
        runs = json.loads(gh('api', f'repos/{repo}/actions/workflows/{workflow}/runs?head_sha={sha}&event={trigger}&per_page=1',
                             '--jq', '[.workflow_runs[] | {id, status}]'))
        if not runs:
            print(f'rerun: no {workflow} run for {sha}')
            return 0
        if runs[0]['status'] == 'completed':
            break
        if time.monotonic() > deadline:
            print(f'rerun: run {runs[0]["id"]} still {runs[0]["status"]} after {WAIT_SECONDS}s')
            return 1
        time.sleep(POLL_SECONDS)
    run_id = runs[0]['id']
    jobs = json.loads(gh('api', f'repos/{repo}/actions/runs/{run_id}/jobs', '--jq', '[.jobs[] | {id, name}]'))
    job = next((j for j in jobs if j['name'] == job_name), None)
    if job is None:
        print(f'rerun: run {run_id} has no job {job_name}')
        return 1
    gh('api', '-X', 'POST', f'repos/{repo}/actions/jobs/{job["id"]}/rerun')
    print(f'rerun: re-ran {job_name} (job {job["id"]}) of run {run_id} for {sha}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
