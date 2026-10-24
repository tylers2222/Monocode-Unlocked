# The E2E image. Build it through `make e2e`, which sends `git archive <commit>`
# as the context, so the image holds exactly one commit and nothing else.
# Layers run from slowest to fastest to change: a source edit rebuilds only the
# last COPY.
FROM ubuntu:24.04@sha256:68ee22004a9109b44973313b02ff7b1de250f1de0f5ecb628d73f579aaff33ac
ARG TARGETARCH
ENV DEBIAN_FRONTEND=noninteractive

# WebKitGTK and the other Tauri build libraries come from the script CI uses.
# Xvfb is for running the Linux app headless.
COPY scripts/install-linux-deps-debian.sh /tmp/install-linux-deps-debian.sh
RUN bash /tmp/install-linux-deps-debian.sh \
 && apt-get install -y --no-install-recommends ca-certificates xauth xvfb xz-utils \
 && rm -rf /var/lib/apt/lists/* /tmp/install-linux-deps-debian.sh

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
COPY .github/rust-version /tmp/rust-version
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
  | sh -s -- -y --no-modify-path --profile minimal --default-toolchain "$(cat /tmp/rust-version)" \
 && rm /tmp/rust-version

# .nvmrc holds the Node major; the newest release of it is checked against
# nodejs.org's SHASUMS256.txt.
COPY .nvmrc /tmp/nvmrc
RUN set -eu; arch=x64; [ "$TARGETARCH" = arm64 ] && arch=arm64; \
    base="https://nodejs.org/dist/latest-v$(cat /tmp/nvmrc).x"; cd /tmp; \
    curl -fsSLO "$base/SHASUMS256.txt"; \
    tarball="$(awk -v a="linux-$arch.tar.xz" '$2 ~ a"$" { print $2 }' SHASUMS256.txt)"; \
    curl -fsSLO "$base/$tarball"; \
    grep " $tarball\$" SHASUMS256.txt | sha256sum -c -; \
    tar -xJf "$tarball" -C /usr/local --strip-components=1 --no-same-owner; \
    rm SHASUMS256.txt "$tarball" nvmrc; \
    node --version

# The omp release ci.yml installs, checked against its SHA256SUMS.txt.
RUN set -eu; case "$TARGETARCH" in \
      amd64) asset=omp-linux-x64 sum=97cf39557bf3d98327dd4c9814380b7e6bcb76e41169edd9b0ff30424c733a2a ;; \
      arm64) asset=omp-linux-arm64 sum=c115f95a0a0081d3724a3c878231fb8c3b2fdb25ad5ad6c3b30d152a82b3939e ;; \
    esac; \
    curl -fsSL -o /usr/local/bin/omp "https://github.com/can1357/oh-my-pi/releases/download/v18.2.11/$asset"; \
    echo "$sum  /usr/local/bin/omp" | sha256sum -c -; \
    chmod 0755 /usr/local/bin/omp; \
    omp --version

ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
RUN useradd --create-home e2e && mkdir /repo /results "$PLAYWRIGHT_BROWSERS_PATH" \
 && chown e2e:e2e /repo /results "$PLAYWRIGHT_BROWSERS_PATH"
WORKDIR /repo

USER e2e
COPY --chown=e2e:e2e package.json package-lock.json ./
RUN npm ci --no-audit --no-fund
USER root
RUN npx playwright install-deps chromium webkit && rm -rf /var/lib/apt/lists/*
USER e2e
RUN npx playwright install chromium webkit

COPY --chown=e2e:e2e . .
ENV CI=true
ENTRYPOINT ["bash", "docker/e2e-run.sh"]
