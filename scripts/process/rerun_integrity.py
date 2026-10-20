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
        # Runs created in the same second come back in any order, and a run cancelled
        # before its jobs started has no job to re-run, so take the newest run (by id)
        # that has the job.
        runs = sorted(json.loads(gh('api', f'repos/{repo}/actions/workflows/{workflow}/runs?head_sha={sha}&event={trigger}&per_page=20',
                                    '--jq', '[.workflow_runs[] | {id, status}]')), key=lambda run: -run['id'])
        with_job = [(run, job) for run in runs for job in json.loads(gh(
            'api', f'repos/{repo}/actions/runs/{run["id"]}/jobs', '--jq', '[.jobs[] | {id, name}]')) if job['name'] == job_name]
        if not with_job:
            print(f'rerun: no {workflow} run with job {job_name} for {sha}')
            return 0
        run, job = with_job[0]
        if run['status'] == 'completed':
            break
        if time.monotonic() > deadline:
            print(f'rerun: run {run["id"]} still {run["status"]} after {WAIT_SECONDS}s')
            return 1
        time.sleep(POLL_SECONDS)
    gh('api', '-X', 'POST', f'repos/{repo}/actions/jobs/{job["id"]}/rerun')
    print(f'rerun: re-ran {job_name} (job {job["id"]}) of run {run["id"]} for {sha}')
    return 0


if __name__ == '__main__':
    sys.exit(main())
