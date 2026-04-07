NAME := cargo-gc
PREFIX := /usr/local
TARGET := target/release
SOURCE := $(wildcard src/*.rs)

install: $(PREFIX)/bin/$(NAME)

$(PREFIX)/bin/$(NAME): $(TARGET)/$(NAME)
	sudo install -o 0 -g 0 -m 0755 $< $(PREFIX)/bin

$(TARGET)/$(NAME): $(SOURCE)
	cargo build --release

.PHONY: install
