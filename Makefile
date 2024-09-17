GH_ORG ?= pando85
VERSION ?= $(shell git rev-parse --short HEAD)
KUBERNETES_VERSION = 1.30
KIND_CLUSTER_NAME = chart-testing
KOPIUM_PATH ?= kopium

.DEFAULT: help
.PHONY: help
help:	## Show this help menu.
	@echo "Usage: make [TARGET ...]"
	@echo ""
	@@egrep -h "#[#]" $(MAKEFILE_LIST) | sed -e 's/\\$$//' | awk 'BEGIN {FS = "[:=].*?#[#] "}; {printf "\033[36m%-20s\033[0m %s\n", $$1, $$2}'
	@echo ""


.PHONY: kopium
kopium:	## install kopium
kopium:
	@if ! command -v $(KOPIUM_PATH) >/dev/null 2>&1; then \
		echo "$(KOPIUM_PATH) not found. Installing..."; \
		cargo install kopium; \
	else \
		echo "$(KOPIUM_PATH) is already installed."; \
	fi

TARGET_CRD_DIR := kaniop_core/src/crd
CRD_DIR := crd
CRD_FILES := $(wildcard $(CRD_DIR)/*.yaml)

.PHONY: $(TARGET_CRD_DIR)/%.rs
$(TARGET_CRD_DIR)/%.rs: $(CRD_DIR)/%.yaml
	@echo "Generating $@ from $<"
	@kopium -f $< > $@

.PHONY: crd-code
crd-code: $(CRD_FILES:$(CRD_DIR)/%.yaml=$(TARGET_CRD_DIR)/%.rs)
crd-code: kopium
crd-code: ## Generate code from CRD definitions
	@echo "CRDs code generation complete."

.PHONY: build
build:	## compile kaniop
build: crd-code
	cargo build --release

.PHONY: lint
lint:	## lint code
lint: crd-code
	cargo clippy --locked --all-targets --all-features -- -D warnings
	cargo fmt -- --check

.PHONY: test
test:	## run tests
test: lint
	cargo test

.PHONY: update-changelog
update-changelog:	## automatically update changelog based on commits
	git cliff -t v$(VERSION) -u -p CHANGELOG.md

.PHONY: publish
publish: crd-code
publish:	## publish crates
	@for package in $(shell find . -mindepth 2 -not -path './vendor/*' -name Cargo.toml -exec dirname {} \; | sort -r);do \
		cd $$package; \
		cargo publish; \
		cd -; \
	done;

.PHONY: image
image: crd-code
image:	## build image
	@docker buildx build --load -t ghcr.io/$(GH_ORG)/kaniop .

.PHONY: push-images
push-images: DOCKER_EXTRA_ARGS ?= --platform linux/amd64,linux/arm64
push-images: crd-code
push-images:	## push images
	$(SUDO) docker buildx build $(DOCKER_EXTRA_ARGS) --push -t ghcr.io/$(GH_ORG)/kaniop .

.PHONY: manifest
manifest:	## replace manifest variables
	sed -i "s#image: ghcr.io/.*kaniop.*#image: ghcr.io/$(GH_ORG)/kaniop:$(VERSION)#g" \
		install/kubernetes/deployment.yaml

.PHONY: kind
e2e: image manifest ## run e2e tests
	kind create cluster --name $(KIND_CLUSTER_NAME) --config .github/kind-cluster-$(KUBERNETES_VERSION).yaml
	kind load --name $(KIND_CLUSTER_NAME) docker-image ghcr.io/$(GH_ORG)/kaniop:$(VERSION)
	kubectl apply -f crd/echo.yaml
	kubectl apply -f install/kubernetes/rbac.yaml
	kubectl apply -f install/kubernetes/deployment.yaml

.PHONY: delete-kind
delete-kind:
	kind delete cluster --name $(KIND_CLUSTER_NAME)
