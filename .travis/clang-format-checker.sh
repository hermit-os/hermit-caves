#!/bin/bash

# This clang-format checker has been adapted from:
# https://github.com/citra-emu/citra/blob/master/.travis/clang-format/script.sh

MY_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

# Check for trailing whitespaces
if grep -u '\s$' CMakeLists.txt README.md LICENSE .gitignore *.c *.h; then
    echo "[ERROR] Found trailing whitespaces. Abort!"
    exit 1
fi

# dertmine changed files (without deletions)
if [ "${TRAVIS_EVENT_TYPE}" = "pull_request" ]; then
    changed_files="$(git diff --name-only --diff-filter=ACMRTUXB $TRAVIS_COMMIT_RANGE | grep '^[^.]*[.]\(c\|h\)$' || true)"
else
    # Check everything for branch pushes
    changed_files="$(find ${MY_DIR} -name '*.c' -or -name '*.h')"
fi

for file in ${changed_files}; do
	file_diff=$(diff -u ${file} <(clang-format-6.0 ${file}) || true)
    if ! [ -z "${file_diff}" ]; then
        echo "[ERROR] ${file} needs to be formated in accordance with the .clang-format. Abort!"
        fail=1
    fi
done

if [ "$fail" = 1 ]; then
    exit 1
fi
