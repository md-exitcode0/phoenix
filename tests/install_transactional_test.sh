#!/usr/bin/env bash

# Failure-injection coverage for install.sh's exchange/backup boundary.
#
# The fixture is a throw-away checkout with a fake Cargo command and a `mv`
# shim that can fail after the exchange itself or when the exchanged old
# destination is moved to the `.prev` slot. It never touches the real
# checkout, ~/.phoenix, or ~/.local.

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
FIXTURE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/phoenix-install-test.XXXXXX")"
trap 'rm -rf -- "$FIXTURE_ROOT"' EXIT

run_case() {
    local with_prior="$1"
    local failure_mode="${2:-backup}"
    local case_root="$FIXTURE_ROOT/case-$with_prior-$failure_mode"
    local fake_repo="$case_root/repo"
    local fake_bin="$case_root/bin"
    local target="$case_root/target"
    local state="$case_root/state"
    local home="$case_root/home"
    local tmpdir="$case_root/tmp"
    local output status stage_path prior_path

    mkdir -p "$fake_repo" "$fake_bin" "$target" "$home" "$tmpdir"
    cp "$SCRIPT_DIR/install.sh" "$fake_repo/install.sh"
    chmod 0755 "$fake_repo/install.sh"
    printf '%s\n' '[package]' 'name = "fixture"' 'version = "0.1.0"' 'edition = "2021"' \
        > "$fake_repo/Cargo.toml"

    cat > "$fake_bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
manifest=''
args=("$@")
for ((index = 0; index < ${#args[@]}; index++)); do
    if [[ "${args[$index]}" == "--manifest-path" ]]; then
        manifest="${args[$((index + 1))]}"
    fi
done
[[ -n "$manifest" ]] || exit 91
repo="$(cd "$(dirname "$manifest")" && pwd -P)"
mkdir -p "$repo/target/release"
printf '%s\n' 'new-binary' > "$repo/target/release/phoenix"
EOF
    chmod 0755 "$fake_bin/cargo"

    cat > "$fake_bin/mv" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
args=("$@")
has_exchange=0
for arg in "${args[@]}"; do
    [[ "$arg" == --exchange ]] && has_exchange=1
done
    if (( ${#args[@]} >= 2 )); then
        source="${args[$(( ${#args[@]} - 2 ))]}"
        destination="${args[$(( ${#args[@]} - 1 ))]}"
        if [[ "${PHOENIX_TEST_FAILURE_MODE:-backup}" == exchange-after \
            && "$(basename "$source")" == .phoenix.stage.* \
            && "$(basename "$destination")" == phoenix ]]; then
            /usr/bin/mv "$@"
            touch "$(dirname "$destination")/.exchange-observed"
            exit 98
        fi
        if [[ "${PHOENIX_TEST_FAILURE_MODE:-backup}" == backup \
            && "$(basename "$source")" == .phoenix.stage.* \
            && "$(basename "$destination")" == phoenix.prev ]]; then
            parent="$(dirname "$destination")"
            [[ "$(< "$source")" == 'old-binary' ]] || {
                printf 'backup-move injection ran before exchange\n' >&2
                exit 96
            }
            [[ "$(< "$parent/phoenix")" == 'new-binary' ]] || {
                printf 'backup-move injection did not observe new destination\n' >&2
                exit 95
            }
            # Complete the rename, then report failure. This is the
            # post-mutation window in which the old destination is at the
            # backup path and the stage path has disappeared.
            /usr/bin/mv "$@"
            touch "$parent/.exchange-observed"
            exit 97
        fi
        if [[ "${PHOENIX_TEST_FAILURE_MODE:-backup}" == published-rollback-exchange-after \
            && "$has_exchange" == 1 \
            && "$(basename "$source")" == templates.prev \
            && "$(basename "$destination")" == templates ]]; then
            /usr/bin/mv "$@"
            touch "$(dirname "$destination")/.published-rollback-exchange-observed"
            exit 94
        fi
        if [[ "${PHOENIX_TEST_FAILURE_MODE:-backup}" == prior-after \
            && "$(basename "$source")" == phoenix.prev \
            && "$(basename "$destination")" == .phoenix.prev.prior.* ]]; then
            # Complete the prior-backup reservation, then report failure.
            # The installer must retain the moved prior path because it has
            # not yet reached a state from which rollback can infer ownership.
            /usr/bin/mv "$@"
            touch "$(dirname "$destination")/.prior-observed"
            exit 92
        fi
        if [[ "${PHOENIX_TEST_FAILURE_MODE:-backup}" == published-rollback-exchange-after \
            && "$(basename "$source")" == .phoenix.stage.* \
            && "$(basename "$destination")" == phoenix ]]; then
            # Fail the later binary publish before it exchanges. This sends
            # the already-published template through the published rollback
            # branch below.
            exit 93
        fi
    fi
exec /usr/bin/mv "$@"
EOF
    chmod 0755 "$fake_bin/mv"

    printf '%s\n' 'old-binary' > "$target/phoenix"
    chmod 0755 "$target/phoenix"
    if [[ "$with_prior" == 1 ]]; then
        printf '%s\n' 'old-backup' > "$target/phoenix.prev"
        chmod 0755 "$target/phoenix.prev"
    fi
    if [[ "$failure_mode" == published-rollback-exchange-after ]]; then
        mkdir -p "$fake_repo/templates" "$state/templates" "$state/templates.prev"
        chmod 0700 "$state"
        printf '%s\n' 'new-template' > "$fake_repo/templates/example.txt"
        printf '%s\n' 'old-template' > "$state/templates/example.txt"
        printf '%s\n' 'old-template-backup' > "$state/templates.prev/example.txt"
        chmod 0700 "$state/templates" "$state/templates.prev"
    fi

    set +e
    output="$(
        cd "$fake_repo" &&
        HOME="$home" TMPDIR="$tmpdir" PHOENIX_HOME="$state" \
            PHOENIX_INSTALL_NO_SYSTEMD_RUN=1 PHOENIX_TEST_FAILURE_MODE="$failure_mode" \
            PATH="$fake_bin:$PATH" \
            bash ./install.sh "$target"
    )" 2>&1
    status=$?
    set -e

    (( status != 0 )) || {
        printf 'expected injected failure (mode=%s prior=%s)\n%s\n' "$failure_mode" "$with_prior" "$output" >&2
        return 1
    }
    if [[ "$failure_mode" != published-rollback-exchange-after && "$failure_mode" != prior-after ]]; then
        [[ -f "$target/.exchange-observed" ]] || {
            printf 'failure injection did not prove exchange occurred (mode=%s prior=%s)\n%s\n' "$failure_mode" "$with_prior" "$output" >&2
            return 1
        }
    fi

    if [[ "$failure_mode" == backup ]]; then
        # The injected second move completed before reporting failure. The
        # transaction must preserve the new destination, old backup, and any
        # reserved prior backup; it cannot safely infer a rollback.
        [[ "$(< "$target/phoenix")" == 'new-binary' ]] || {
            printf 'post-mutation backup failure lost new destination (prior=%s)\n%s\n' "$with_prior" "$output" >&2
            return 1
        }
        [[ "$(< "$target/phoenix.prev")" == 'old-binary' ]] || {
            printf 'post-mutation backup failure lost old destination backup (prior=%s)\n%s\n' "$with_prior" "$output" >&2
            return 1
        }
        if [[ "$with_prior" == 1 ]]; then
            prior_path="$(find "$target" -maxdepth 1 -type f -name '.phoenix.prev.prior.*' -print -quit)"
            [[ -n "$prior_path" && "$(< "$prior_path")" == 'old-backup' ]] || {
                printf 'post-mutation backup failure lost prior backup\n%s\n' "$output" >&2
                return 1
            }
        fi
        if find "$target" -maxdepth 1 -type f -name '.phoenix.stage.*' -print -quit | grep -q .; then
            printf 'post-mutation backup failure left an unexpected stage (prior=%s)\n%s\n' "$with_prior" "$output" >&2
            return 1
        fi
    elif [[ "$failure_mode" == exchange-after ]]; then
        [[ "$(< "$target/phoenix")" == 'new-binary' ]] || {
            printf 'exchange-failure case lost new destination (prior=%s)\n%s\n' "$with_prior" "$output" >&2
            return 1
        }
        stage_path="$(find "$target" -maxdepth 1 -type f -name '.phoenix.stage.*' -print -quit)"
        [[ -n "$stage_path" && "$(< "$stage_path")" == 'old-binary' ]] || {
            printf 'exchange-failure case did not preserve old stage (prior=%s)\n%s\n' "$with_prior" "$output" >&2
            return 1
        }
        if [[ "$with_prior" == 1 ]]; then
            prior_path="$(find "$target" -maxdepth 1 -type f -name '.phoenix.prev.prior.*' -print -quit)"
            [[ -n "$prior_path" && "$(< "$prior_path")" == 'old-backup' ]] || {
                printf 'exchange-failure case did not preserve prior backup\n%s\n' "$output" >&2
                return 1
            }
        fi
    elif [[ "$failure_mode" == published-rollback-exchange-after ]]; then
        [[ -f "$state/.published-rollback-exchange-observed" ]] || {
            printf 'published rollback injection did not prove exchange occurred (prior=%s)\n%s\n' "$with_prior" "$output" >&2
            return 1
        }
        [[ "$(< "$state/templates/example.txt")" == 'old-template' ]] || {
            printf 'published rollback lost the original destination (prior=%s)\n%s\n' "$with_prior" "$output" >&2
            return 1
        }
        [[ "$(< "$state/templates.prev/example.txt")" == 'new-template' ]] || {
            printf 'published rollback did not preserve the current backup (prior=%s)\n%s\n' "$with_prior" "$output" >&2
            return 1
        }
        prior_path="$(find "$state" -maxdepth 1 -type d -name '.templates.prev.prior.*' -print -quit)"
        [[ -n "$prior_path" && "$(< "$prior_path/example.txt")" == 'old-template-backup' ]] || {
            printf 'published rollback did not preserve the prior backup (prior=%s)\n%s\n' "$with_prior" "$output" >&2
            return 1
        }
    else
        [[ -f "$target/.prior-observed" ]] || {
            printf 'prior-backup injection did not prove the move occurred (prior=%s)\n%s\n' "$with_prior" "$output" >&2
            return 1
        }
        [[ "$with_prior" == 1 ]] || {
            printf 'prior-backup case unexpectedly ran without a prior backup\n%s\n' "$output" >&2
            return 1
        }
        [[ "$(< "$target/phoenix")" == 'old-binary' ]] || {
            printf 'prior-backup failure changed the current destination\n%s\n' "$output" >&2
            return 1
        }
        [[ ! -e "$target/phoenix.prev" ]] || {
            printf 'prior-backup failure unexpectedly recreated the backup slot\n%s\n' "$output" >&2
            return 1
        }
        prior_path="$(find "$target" -maxdepth 1 -type f -name '.phoenix.prev.prior.*' -print -quit)"
        [[ -n "$prior_path" && "$(< "$prior_path")" == 'old-backup' ]] || {
            printf 'prior-backup failure lost the moved prior backup\n%s\n' "$output" >&2
            return 1
        }
        stage_path="$(find "$target" -maxdepth 1 -type f -name '.phoenix.stage.*' -print -quit)"
        [[ -n "$stage_path" && "$(< "$stage_path")" == 'new-binary' ]] || {
            printf 'prior-backup failure lost the new staged binary\n%s\n' "$output" >&2
            return 1
        }
    fi
}

run_case 0 backup
run_case 1 backup
run_case 0 exchange-after
run_case 1 exchange-after
run_case 0 published-rollback-exchange-after
run_case 1 published-rollback-exchange-after
run_case 1 prior-after
printf '%s\n' 'install transactional failure injection: ok'
