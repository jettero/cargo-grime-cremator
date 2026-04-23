NAME := cargo-gc
PREFIX := /usr/local
TARGET := target/release
SOURCE := $(wildcard src/*.rs) Cargo.toml Cargo.lock Makefile

install: $(PREFIX)/bin/$(NAME)
	$(NAME) --version

$(PREFIX)/bin/$(NAME): $(TARGET)/$(NAME)
	sudo install -v -o 0 -g 0 -m 0755 $< $(PREFIX)/bin

$(TARGET)/$(NAME): $(SOURCE)
	cargo build --release

.PHONY: install
