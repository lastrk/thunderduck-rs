# Root Makefile — CI-only shim for .github/workflows/extension-release.yml.
#
# DuckDB's reusable `_extension_distribution.yml` (extension-ci-tools) assumes
# the extension IS the repository root: it checks this repo out at the runner's
# workspace root, clones extension-ci-tools/ NEXT TO it (this is the workflow's
# own clone at `ci_tools_version`, NOT the extension/extension-ci-tools
# submodule), and drives everything with bare `make <target>` at the repo
# root. It has no subdirectory input, and several of its targets hardcode
# root-relative paths (`set_duckdb_version` does `cd duckdb`, tests glob
# `test/*`). This shim adapts that harness to the in-tree extension:
#
#   - EXT_CONFIG points into extension/
#   - the tracked root symlink `duckdb -> extension/duckdb` satisfies the
#     hardcoded ./duckdb paths (the submodule is initialized by the workflow's
#     recursive checkout)
#   - TESTS_BASE_DIRECTORY / VCPKG_MANIFEST_FLAGS are re-pointed after the
#     include (the include overwrites/derives them from root-relative
#     assumptions)
#
# For LOCAL extension builds do NOT use this file — use
# scripts/dev/build-extension.sh, which drives extension/Makefile against the
# extension/extension-ci-tools submodule and asserts the three-way version
# lock. The guard below makes any local invocation fail with that pointer.

PROJ_DIR := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

EXT_NAME=thdck_spark_funcs
EXT_CONFIG=${PROJ_DIR}extension/extension_config.cmake

ifeq ($(wildcard extension-ci-tools/makefiles/duckdb_extension.Makefile),)
$(error this root Makefile is a CI-only shim for extension-release.yml, which clones extension-ci-tools/ at the repo root; for local extension builds use scripts/dev/build-extension.sh)
endif

include extension-ci-tools/makefiles/duckdb_extension.Makefile

# The include hardcodes these to root-relative locations ("test/", PROJ_DIR);
# re-point them at the extension subtree. Both are consumed lazily by the
# recipes, so post-include reassignment takes effect.
TESTS_BASE_DIRECTORY = extension/test/
ifneq ("${VCPKG_TOOLCHAIN_PATH}", "")
VCPKG_MANIFEST_FLAGS := -DVCPKG_MANIFEST_DIR='${PROJ_DIR}extension'
endif
