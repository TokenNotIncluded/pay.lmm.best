ARCH := $(shell uname -m)
TARGET ?= $(ARCH)-unknown-linux-musl
.PHONY: check build package image clean
check:
	cargo fmt --all -- --check
	cargo test --locked --all-targets
	cargo clippy --locked --all-targets -- -D warnings
	python3 -m unittest discover -s packaging/tests -v
build:
	bash packaging/build.sh $(TARGET)
package: build
	python3 packaging/package.py build --target $(TARGET)
image: package
	docker build --build-arg VERSION=$$(python3 packaging/package.py version) --build-arg REVISION=$$(git rev-parse HEAD) -t pay-lmm:local .
clean:
	rm -rf dist
