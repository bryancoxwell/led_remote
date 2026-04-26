CARGO ?= cargo

.PHONY: build deb

build:
	$(CARGO) build --release

deb: build
	$(CARGO) deb --no-build
