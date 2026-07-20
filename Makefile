PACKAGES=by-macros dioxus-gtag-macro dioxus-gtag ts-server-fn-client ts-server-fn ts-server-fn-axum

setup:
	cargo install mdbook

.PHONY: publish
publish: $(patsubst %,publish.%,$(PACKAGES))

publish.%:
	./publish.sh $*

.PHONY: build
build: $(patsubst %,build.%,$(PACKAGES))

build.%:
	cargo build -p $*

.PHONY: docs
docs:
	mdbook serve

.PHONY: test.e2e
test.e2e:
	cd fixtures/ts-server-fn-e2e/service && cargo test && cargo check --no-default-features
	cd fixtures/ts-server-fn-e2e/client && cargo test
