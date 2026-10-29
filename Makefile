# The container shares the host (Docker VM) PID namespace so rtop sees a real
# process tree, and runs privileged so the namespace/D-state labs work.
# Linux build artefacts live in a named volume, separate from the macOS target/.
IMAGE := rtop-dev
DOCKER_RUN := docker run --rm --pid=host --privileged \
	-v $(CURDIR):/src -v rtop-target:/target -v rtop-cargo:/usr/local/cargo/registry \
	-w /src $(IMAGE)

.PHONY: run test lint image linux-shell linux-test linux-run linux-up linux-exec linux-down

run:
	cargo run --release

test:
	cargo test

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

image:
	docker build -t $(IMAGE) .

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
