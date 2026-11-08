# SPDX-License-Identifier: GPL-3.0-only
#
# Host tests for the hardware independent modules of the migration firmware.
#   make check     build with sanitizers and run the tests
#   make coverage  run the tests with gcov and enforce 100% line/branch/function coverage

CC ?= cc
SRC := $(abspath ../src/mg2x_sha256.c ../src/mg2x_job.c ../src/mg2x_writer.c)
WARNINGS := -Wall -Wextra -Werror -Wconversion -Wsign-conversion -Wshadow -Wformat=2 -Wpedantic
CFLAGS := -std=c11 -O1 -g $(WARNINGS)
BUILD := build
TEST := $(abspath test_main.c)

.PHONY: check coverage clean

check: $(BUILD)/test_sanitized
	./$(BUILD)/test_sanitized

$(BUILD)/test_sanitized: test_main.c $(SRC) ../src/*.h
	mkdir -p $(BUILD)
	$(CC) $(CFLAGS) -fsanitize=address,undefined -fno-sanitize-recover=all -o $@ test_main.c $(SRC)

coverage: clean
	mkdir -p $(BUILD)
	$(CC) $(CFLAGS) -O0 --coverage -o $(BUILD)/test_coverage $(TEST) $(SRC)
	./$(BUILD)/test_coverage
	gcovr --root $(abspath ..) --filter $(abspath ../src) \
	  --object-directory $(BUILD) --exclude-throw-branches --fail-under-line 100 --fail-under-branch 100 --fail-under-function 100 --print-summary

clean:
	rm -rf $(BUILD) *.gcda *.gcno
