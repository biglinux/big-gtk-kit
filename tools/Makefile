# SPDX-FileCopyrightText: 2026 BigLinux contributors
# SPDX-License-Identifier: MIT
CC ?= cc
CFLAGS ?= -O2 -g
WARN = -Wall -Wextra -Werror
GLIB = $(shell pkg-config --cflags --libs gobject-2.0 glib-2.0)

all: build/libgobject-census.so build/reftrace.so

build/libgobject-census.so: census/gobject-census.c census/gobject-census.h
	@mkdir -p build
	$(CC) $(CFLAGS) $(WARN) -shared -fPIC -o $@ census/gobject-census.c $(GLIB) -ldl

build/reftrace.so: reftrace/reftrace.c
	@mkdir -p build
	$(CC) $(CFLAGS) $(WARN) -shared -fPIC -o $@ reftrace/reftrace.c $(GLIB) -ldl

build/census-control: census/tests/control.c build/libgobject-census.so
	$(CC) $(CFLAGS) $(WARN) -Icensus -o $@ census/tests/control.c -Lbuild -lgobject-census -Wl,-rpath,'$$ORIGIN' $(GLIB)

check: build/census-control
	build/census-control build
	python3 -m unittest discover -s static/tests
	python3 -m unittest discover -s sweep/tests

# The census answering SIGUSR2 inside a real GTK program: needs kwin_wayland, so
# it is not part of `check`.
check-gtk: build/libgobject-census.so
	@mkdir -p build/gtk-signal
	./headless-gate.sh env GOBJECT_CENSUS_OUT=$(CURDIR)/build/gtk-signal/c \
		LD_PRELOAD=$(CURDIR)/build/libgobject-census.so python3 census/tests/gtk_signal.py build/gtk-signal

clean:
	rm -rf build

.PHONY: all check check-gtk clean
