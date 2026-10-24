## E2E suites in one Linux Docker image, run in the background or attached.
##
##   make e2e [SUITE=all] [REV=HEAD] [CPUS=4] [MEMORY=6g] [WAIT=1]
##       Build the image for commit REV (committed files only) unless it is
##       already built, start the suites detached, and print the run id.
##       WAIT=1 prints the id, then runs attached and exits with the suites'
##       code (CI: .github/workflows/e2e.yml). SUITE is all, browser, evals,
##       milestones or scenarios; milestones and scenarios are not ported yet
##       and record a skipped row. CPUS and MEMORY cap the container.
##   make e2e-status RUN=<id>
##       Say if the run is still going. Once it ends, copy its results to
##       e2e-results/<id>/ (results.tsv, junit.xml, run.log, browser/ with
##       screenshots and traces, and done holding the exit code), then summarise.
##   make e2e-logs RUN=<id>
##       The run's output so far.
##
## On another machine: any host with Docker that you can reach over ssh.
##   docker context create e2e-box --docker host=ssh://<user>@<host>
##   DOCKER_CONTEXT=e2e-box make e2e SUITE=browser
##   make e2e-status RUN=<id>
## The image builds and runs on that host. The run remembers its context, so
## e2e-status and e2e-logs need no DOCKER_CONTEXT, and docker cp brings the
## results back to e2e-results/<id>/ here.

SUITE ?= all
REV ?= HEAD
CPUS ?= 4
MEMORY ?= 6g
WAIT ?=

.PHONY: help e2e e2e-status e2e-logs

help:
	@sed -n 's/^## \{0,1\}//p' $(firstword $(MAKEFILE_LIST))

e2e:
	@bash docker/e2e.sh start '$(SUITE)' '$(REV)' '$(CPUS)' '$(MEMORY)' '$(WAIT)'

e2e-status:
	@bash docker/e2e.sh status '$(RUN)'

e2e-logs:
	@bash docker/e2e.sh logs '$(RUN)'
