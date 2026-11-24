# The container shares the host (Docker VM) PID namespace so rtop sees a real
# process tree, and runs privileged so the namespace/D-state labs work.
# Linux build artefacts live in a named volume, separate from the macOS target/.
IMAGE := rtop-dev
DOCKER_RUN := docker run --rm --pid=host --privileged \
	-v $(CURDIR):/src -v rtop-target:/target -v rtop-cargo:/usr/local/cargo/registry \
	-w /src $(IMAGE)

.PHONY: run install test lint demo image linux-shell linux-test linux-run linux-up linux-exec linux-down kube-up kube-e2e kube-down

run:
	cargo run --release

test:
	cargo test
	cargo test --no-default-features

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

image:
	docker build -t $(IMAGE) .

# README GIF (assets/demo.tape). Recorded in a throwaway Linux container, never on the host:
# rtop would show the host's user, IP, battery and apps.
demo: image
	$(DOCKER_RUN) sh -c 'cargo build --release && mkdir -p target/demo && cp /target/release/rtop target/demo/rtop'
	docker run --rm --hostname rtop-demo -v $(CURDIR):/vhs \
		-v $(CURDIR)/target/demo/rtop:/usr/local/bin/rtop:ro ghcr.io/charmbracelet/vhs assets/demo.tape

linux-shell: image
	docker run -it --rm --pid=host --privileged \
		-v $(CURDIR):/src -v rtop-target:/target -v rtop-cargo:/usr/local/cargo/registry \
		-w /src $(IMAGE) bash

linux-run: image
	docker run -it --rm --pid=host --privileged \
		-v $(CURDIR):/src -v rtop-target:/target -v rtop-cargo:/usr/local/cargo/registry \
		-w /src $(IMAGE) cargo run --release

linux-test: image
	$(DOCKER_RUN) cargo test

# A long-lived container named `rtop` that shows up in `docker ps`; attach
# to it as often as you like with `make linux-exec`.
linux-up: image
	docker rm -f rtop >/dev/null 2>&1 || true
	docker run -d --name rtop --pid=host --privileged \
		-v $(CURDIR):/src -v rtop-target:/target -v rtop-cargo:/usr/local/cargo/registry \
		-w /src $(IMAGE) sleep infinity

linux-exec:
	docker exec -it rtop bash

linux-down:
	docker rm -f rtop

# Local kind cluster for Kubernetes mode. Every kubectl call names the
# kind-rtop context explicitly so nothing ever reaches another cluster.
KUBE_CTX := kind-rtop

kube-up:
	kind get clusters | grep -qx rtop || kind create cluster --name rtop
	@# A new namespace gets its default service account a moment later, and
	@# pods are refused until then: retry the apply.
	for i in 1 2 3 4 5; do kubectl --context $(KUBE_CTX) apply -f tests/kube/workloads.yaml && break; sleep 2; done
	kubectl --context $(KUBE_CTX) -n rtop-demo wait --for=condition=Ready pod --all --timeout=180s

kube-e2e: kube-up
	cargo build --release
	@echo "Cluster ready. Walk through specs/001-k8s-targets/quickstart.md with --context $(KUBE_CTX)."

kube-down:
	kind delete cluster --name rtop

# `rtop` on the PATH: a symlink to the release build, so every
# `cargo build --release` updates the command too.
BIN_DIR ?= $(HOME)/.local/bin

install:
	cargo build --release
	mkdir -p $(BIN_DIR)
	ln -sf $(CURDIR)/target/release/rtop $(BIN_DIR)/rtop
	@echo "rtop -> $(CURDIR)/target/release/rtop"
