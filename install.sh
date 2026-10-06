#!/usr/bin/env bash
# Build and install the Phoenix CLI.
# Usage: ./install.sh [-j JOBS] [target_dir] | --design-runtime-only
# Default target: ~/.local/bin

set -Eeuo pipefail
shopt -s nullglob

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN_TAG="${EPOCHSECONDS:-$(date +%s)}.$$"
DEFAULT_TARGET="$HOME/.local/bin"
DEFAULT_JOBS="${PHOENIX_INSTALL_JOBS:-3}"
DEFAULT_STATE_ROOT="$HOME/.phoenix"

TARGET="$DEFAULT_TARGET"
CARGO_JOBS="$DEFAULT_JOBS"
TARGET_WAS_SET=0
JOBS_WAS_SET=0
DESIGN_RUNTIME_ONLY=0

EXT_UUID="phoenix-cursor@phoenix.dev"
EXT_SRC="$SCRIPT_DIR/desktop/gnome-extension/$EXT_UUID"
EXT_DEST="$HOME/.local/share/gnome-shell/extensions/$EXT_UUID"

PROMPTS_SRC="$SCRIPT_DIR/prompts"
PROMPTS_DEST=""

TEMPLATES_SRC="$SCRIPT_DIR/templates"
TEMPLATES_DEST=""

DESIGN_SRC="$SCRIPT_DIR/vendor/tastecode-design"
DESIGN_DEST=""
DESIGN_NODE="${PHOENIX_NODE:-node}"

BUILD_ARTIFACT="$SCRIPT_DIR/target/release/phoenix"
BUILD_MANIFEST="$SCRIPT_DIR/Cargo.toml"

declare -a CLEANUP_PATHS=()
declare -a ROLLBACK_DESTS=()
declare -a ROLLBACK_BACKUPS=()
declare -a ROLLBACK_PRIORS=()
declare -a ROLLBACK_CREATED=()
declare -a ROLLBACK_STAGES=()
declare -a ROLLBACK_STATES=()
ROLLBACK_NEEDED=0
ROLLBACK_FAILED=0
COMMIT_COMPLETE=0

usage() {
    cat <<'EOF'
Usage: ./install.sh [-j JOBS] [target_dir]
       ./install.sh --design-runtime-only

Options:
  -j, --jobs N          Cargo parallelism (default: 3, overridable via PHOENIX_INSTALL_JOBS)
  --design-runtime-only Install only the checked Iris runtime package; no build or role sync
  -h, --help            Show this help

Runtime-only installation uses PHOENIX_HOME (default: ~/.phoenix), requires Node.js
22 or newer (PHOENIX_NODE), and cannot be combined with --jobs or target_dir.
EOF
}

say() {
    printf '%s\n' "$*"
}

die() {
    printf '✗ %s\n' "$*" >&2
    exit 1
}

is_positive_integer() {
    [[ "$1" =~ ^[1-9][0-9]*$ ]]
}

abs_path() {
    local path="$1"
    local current="/" comp
    local raw_base raw_path
    local -a parts=()

    if [[ "$path" == /* ]]; then
        raw_path="$path"
    else
        raw_base="$(pwd -P)"
        raw_path="$raw_base/$path"
    fi

    IFS='/' read -r -a parts <<< "${raw_path#/}"
    for comp in "${parts[@]}"; do
        case "$comp" in
            ""|".")
                continue
                ;;
            "..")
                if [[ "$current" != "/" ]]; then
                    current="$(dirname "$current")"
                fi
                ;;
            *)
                current="${current%/}/$comp"
                [[ ! -L "$current" ]] || die "refusing to follow symlinked path component: $current"
                ;;
        esac
    done

    if [[ -n "$current" ]]; then
        printf '%s\n' "$current"
    else
        printf '/\n'
    fi
}

trim_whitespace() {
    local value="$1"

    value="${value#"${value%%[![:space:]]*}"}"
    value="${value%"${value##*[![:space:]]}"}"
    printf '%s\n' "$value"
}

resolve_state_root() {
    local raw_root default_root state_root home_root temp_root current_root mode
    local comp
    local -a parts=()

    raw_root="$(trim_whitespace "${PHOENIX_HOME:-}")"
    default_root="$(abs_path "$DEFAULT_STATE_ROOT")" || return 1
    if [[ -z "$raw_root" ]]; then
        printf '%s\n' "$default_root"
        return 0
    fi

    [[ "$raw_root" == /* ]] || die "PHOENIX_HOME must be a dedicated absolute directory: $raw_root"
    [[ "$raw_root" != *$'\n'* && "$raw_root" != *$'\r'* ]] \
        || die "PHOENIX_HOME must not contain line breaks"
    IFS='/' read -r -a parts <<< "${raw_root#/}"
    for comp in "${parts[@]}"; do
        [[ "$comp" != "." && "$comp" != ".." ]] \
            || die "PHOENIX_HOME must not contain '.' or '..' path components: $raw_root"
    done

    state_root="$(abs_path "$raw_root")" || return 1
    home_root="$(abs_path "$HOME")" || return 1
    temp_root="$(abs_path "${TMPDIR:-/tmp}")" || return 1
    current_root="$(pwd -P)"
    case "$state_root" in
        /|"$home_root"|"$temp_root"|"$current_root")
            die "refusing to mutate dangerously broad PHOENIX_HOME '$state_root'; choose a dedicated absolute directory"
            ;;
    esac

    if [[ -e "$state_root" ]]; then
        [[ -d "$state_root" ]] || die "PHOENIX_HOME is not a directory: $state_root"
        if [[ "$state_root" != "$default_root" ]]; then
            [[ -O "$state_root" ]] || die "refusing PHOENIX_HOME not owned by the current user: $state_root"
            mode="$(stat -c '%a' -- "$state_root")"
            (( (8#$mode & 8#077) == 0 )) \
                || die "custom PHOENIX_HOME must already be owner-only (0700): $state_root"
        fi
    fi

    printf '%s\n' "$state_root"
}

ensure_safe_dir_path() {
    local raw_path="$1"
    local mode="$2"
    local path current="/" comp
    local -a parts=()

    path="$(abs_path "$raw_path")" || return 1
    IFS='/' read -r -a parts <<< "${path#/}"
    for comp in "${parts[@]}"; do
        [[ -n "$comp" ]] || continue
        [[ "$comp" != "." && "$comp" != ".." ]] || die "refusing path with dot-segments: $path"
        current="${current%/}/$comp"
        [[ ! -L "$current" ]] || die "refusing to follow symlinked path component: $current"
        if [[ -e "$current" ]]; then
            [[ -d "$current" ]] || die "expected directory path but found non-directory: $current"
        else
            mkdir -m "$mode" "$current" || return 1
        fi
    done
    printf '%s\n' "$path"
}

ensure_safe_file_slot() {
    local raw_path="$1"
    local parent_mode="$2"
    local path parent

    path="$(abs_path "$raw_path")"
    parent="$(dirname "$path")"
    ensure_safe_dir_path "$parent" "$parent_mode" >/dev/null
    [[ ! -L "$path" ]] || die "refusing to overwrite symlinked file target: $path"
    if [[ -e "$path" ]]; then
        [[ -f "$path" ]] || die "expected regular file target at $path"
    fi
    printf '%s\n' "$path"
}

ensure_safe_dir_slot() {
    local raw_path="$1"
    local parent_mode="$2"
    local path parent

    path="$(abs_path "$raw_path")" || return 1
    parent="$(dirname "$path")"
    ensure_safe_dir_path "$parent" "$parent_mode" >/dev/null || return 1
    [[ ! -L "$path" ]] || die "refusing to replace symlinked directory target: $path"
    if [[ -e "$path" ]]; then
        [[ -d "$path" ]] || die "expected directory target at $path"
    fi
    printf '%s\n' "$path"
}

ensure_backup_slot_kind() {
    local raw_path="$1"
    local kind="$2"
    local path

    path="$(abs_path "$raw_path")" || return 1
    [[ ! -L "$path" ]] || die "refusing to touch symlinked backup path: $path"
    if [[ -e "$path" ]]; then
        case "$kind" in
            file) [[ -f "$path" ]] || die "expected file backup slot at $path" ;;
            dir) [[ -d "$path" ]] || die "expected directory backup slot at $path" ;;
            *) die "internal error: unknown backup slot kind '$kind'" ;;
        esac
    fi
    printf '%s\n' "$path"
}

validate_source_tree() {
    local src="$1"
    local label="$2"
    local bad

    [[ -d "$src" ]] || die "$label source directory is missing: $src"
    bad="$(find -P "$src" ! -type d ! -type f -print -quit)"
    [[ -z "$bad" ]] || die "$label source contains unsupported path type: $bad"
}

validate_regular_source_file() {
    local path="$1"
    local label="$2"

    [[ -f "$path" ]] || die "$label source file is missing: $path"
    [[ ! -L "$path" ]] || die "$label source file is a symlink: $path"
}

register_cleanup() {
    [[ -n "$1" ]] || die "refusing to register an empty cleanup path"
    CLEANUP_PATHS+=("$1")
}

make_stage_dir() {
    local dest="$1"
    local mode="$2"
    local parent base stage

    parent="$(dirname "$dest")"
    base="$(basename "$dest")"
    stage="$(mktemp -d "$parent/.${base}.stage.${RUN_TAG}.XXXXXX")" || return 1
    chmod "$mode" "$stage" || return 1
    register_cleanup "$stage"
    printf '%s\n' "$stage"
}

make_stage_file() {
    local dest="$1"
    local parent base stage

    parent="$(dirname "$dest")"
    base="$(basename "$dest")"
    stage="$(mktemp "$parent/.${base}.stage.${RUN_TAG}.XXXXXX")"
    register_cleanup "$stage"
    printf '%s\n' "$stage"
}

backup_path_for() {
    local dest="$1"
    printf '%s/%s.prev\n' "$(dirname "$dest")" "$(basename "$dest")"
}

reserve_sibling_path() {
    local dest="$1"
    local label="$2"
    local parent base candidate counter=0

    parent="$(dirname "$dest")"
    base="$(basename "$dest")"
    while :; do
        candidate="$parent/.${base}.${label}.${RUN_TAG}.${counter}"
        if [[ ! -e "$candidate" && ! -L "$candidate" ]]; then
            printf '%s\n' "$candidate"
            return 0
        fi
        counter=$((counter + 1))
    done
}

remove_safe_path() {
    local raw_path="$1"
    local path

    [[ -n "$raw_path" ]] || die "refusing to remove an empty cleanup path"
    path="$(abs_path "$raw_path")" || return 1
    [[ -e "$path" || -L "$path" ]] || return 0
    [[ "$path" != "/" ]] || die "refusing to remove /"
    [[ ! -L "$path" ]] || die "refusing to remove symlink: $path"
    rm -rf --one-file-system -- "$path"
}

probe_exchange_support() {
    local dest="$1"
    local kind="$2"
    local parent left right left_marker right_marker help

    parent="$(dirname "$dest")"
    help="$(mv --help 2>&1)" || {
        die "cannot inspect mv exchange support before upgrading $dest"
    }
    [[ "$help" == *"--exchange"* ]] || {
        die "upgrading $dest requires an mv implementation with --exchange support"
    }

    case "$kind" in
        file)
            left="$(mktemp "$parent/.phoenix.exchange-probe.XXXXXX")"
            right="$(mktemp "$parent/.phoenix.exchange-probe.XXXXXX")"
            left_marker="$left"
            right_marker="$right"
            ;;
        dir)
            left="$(mktemp -d "$parent/.phoenix.exchange-probe.XXXXXX")"
            right="$(mktemp -d "$parent/.phoenix.exchange-probe.XXXXXX")"
            left_marker="$left/.marker"
            right_marker="$right/.marker"
            ;;
        *)
            die "internal error: unknown exchange probe kind '$kind'"
            ;;
    esac
    register_cleanup "$left"
    register_cleanup "$right"
    printf 'left\n' > "$left_marker"
    printf 'right\n' > "$right_marker"

    if ! mv -T --no-copy --exchange "$left" "$right"; then
        die "cannot atomically exchange $dest on this filesystem; refusing an in-place upgrade"
    fi
    [[ "$(< "$left_marker")" == right && "$(< "$right_marker")" == left ]] || {
        die "mv --exchange did not swap probe paths for $dest; refusing an in-place upgrade"
    }
    remove_safe_path "$left"
    remove_safe_path "$right"
}

record_publish() {
    local dest="$1"
    local backup="$2"
    local prior="$3"
    local created="$4"
    local stage="$5"
    local state="$6"

    ROLLBACK_DESTS+=("$dest")
    ROLLBACK_BACKUPS+=("$backup")
    ROLLBACK_PRIORS+=("$prior")
    ROLLBACK_CREATED+=("$created")
    ROLLBACK_STAGES+=("$stage")
    ROLLBACK_STATES+=("$state")
    ROLLBACK_NEEDED=1
}

restore_prior_backup() {
    local backup="$1"
    local prior="$2"

    [[ -n "$prior" && -e "$prior" ]] || return 0
    if [[ -e "$backup" ]]; then
        if ! remove_safe_path "$backup"; then
            ROLLBACK_FAILED=1
            return 1
        fi
    fi
    if ! mv -T --no-copy "$prior" "$backup"; then
        ROLLBACK_FAILED=1
        return 1
    fi
}

publish_staged_path() {
    local label="$1"
    local stage="$2"
    local dest="$3"
    local kind="$4"
    local backup prior="" record_index

    backup="$(backup_path_for "$dest")"
    ensure_backup_slot_kind "$backup" "$kind" >/dev/null

    if [[ -e "$dest" ]]; then
        probe_exchange_support "$dest" "$kind"
        if [[ -e "$backup" ]]; then
            prior="$(reserve_sibling_path "$backup" "prior")"
            register_cleanup "$prior"
            # Journal the reservation before moving it. mv can rename the
            # prior backup and then return nonzero; without a record, the
            # EXIT cleanup would remove the only copy of that backup.
            record_publish "$dest" "$backup" "$prior" "0" "$stage" "prior_pending"
            record_index=$((${#ROLLBACK_STATES[@]} - 1))
            mv -T --no-copy "$backup" "$prior"
            ROLLBACK_STATES[$record_index]="exchange_pending"
        else
            # Record the pre-exchange state before the first mutating rename.
            # If the exchange succeeds but the second move (old destination
            # → .prev) fails, the stage path contains the old destination.
            # Without this intermediate record the EXIT trap treats that path
            # as disposable scratch and can delete the only copy of the
            # previous install.
            record_publish "$dest" "$backup" "$prior" "0" "$stage" "exchange_pending"
            record_index=$((${#ROLLBACK_STATES[@]} - 1))
        fi
        # Until mv has returned successfully, its outcome is ambiguous: a
        # process may be interrupted after renameat2(RENAME_EXCHANGE) but
        # before mv reports its status. Preserve both paths in that case.
        mv -T --no-copy --exchange "$stage" "$dest"
        # The old destination is now either still at $stage (if the next
        # rename has not happened) or already at $backup. Until that rename
        # returns successfully its result is ambiguous as well: mv can
        # complete the rename and then report a nonzero status. Preserve all
        # paths in that window rather than letting the exchanged rollback
        # path discard whichever copy is still valuable.
        ROLLBACK_STATES[$record_index]="publish_pending"
        mv -T --no-copy "$stage" "$backup"
        ROLLBACK_STATES[$record_index]="published"
    else
        record_publish "$dest" "" "" "1" "$stage" "created_pending"
        record_index=$((${#ROLLBACK_STATES[@]} - 1))
        mv -T --no-copy "$stage" "$dest"
        ROLLBACK_STATES[$record_index]="created"
    fi

    say "✓ $label"
}

rollback_changes() {
    local index dest backup prior created stage state rollback_published_exchange rollback_exchange

    (( ROLLBACK_NEEDED )) || return 0
    say "Rolling back incomplete install..."
    for (( index=${#ROLLBACK_DESTS[@]} - 1; index>=0; index-- )); do
        dest="${ROLLBACK_DESTS[$index]}"
        backup="${ROLLBACK_BACKUPS[$index]}"
        prior="${ROLLBACK_PRIORS[$index]}"
        created="${ROLLBACK_CREATED[$index]}"
        stage="${ROLLBACK_STAGES[$index]}"
        state="${ROLLBACK_STATES[$index]}"

        case "$state" in
            created_pending)
                # The destination was absent and the staged file was never
                # published. Leave the destination untouched and discard only
                # the still-new stage.
                if [[ -e "$stage" ]] && ! remove_safe_path "$stage"; then
                    ROLLBACK_FAILED=1
                fi
                ;;
            created)
                if [[ -e "$dest" ]] && ! remove_safe_path "$dest"; then
                    ROLLBACK_FAILED=1
                fi
                ;;
            prepared)
                # Existing destination was not exchanged. Restore any prior
                # backup reservation and discard the new staged tree.
                if [[ -e "$stage" ]] && ! remove_safe_path "$stage"; then
                    ROLLBACK_FAILED=1
                fi
                restore_prior_backup "$backup" "$prior" || true
                ;;
            exchange_pending)
                # mv may have completed the exchange before returning a
                # failure (or the shell may have been interrupted while mv
                # was reporting its status). The destination and stage paths
                # are therefore both potentially valuable; do not guess
                # which one contains the old install or run scratch cleanup.
                say "Exchange outcome was ambiguous for $dest; preserving destination and stage for manual recovery." >&2
                ROLLBACK_FAILED=1
                ;;
            prior_pending)
                # The existing backup may have been moved to $prior, or the
                # move may not have happened at all. Keep both names and the
                # staged/new destination untouched until an operator can
                # determine which side of the rename completed.
                say "Prior-backup reservation outcome was ambiguous for $dest; preserving destination, backup, prior, and stage paths for manual recovery." >&2
                ROLLBACK_FAILED=1
                ;;
            publish_pending)
                # The first exchange succeeded, but the move of the old
                # destination into the backup slot did not return success.
                # It may have happened before mv reported its status, so the
                # old tree may be at either $stage or $backup. Preserve every
                # path, including a reserved prior backup, for manual recovery.
                say "Backup publication outcome was ambiguous for $dest; preserving destination, stage, backup, and prior paths for manual recovery." >&2
                ROLLBACK_FAILED=1
                ;;
            exchanged)
                # The exchange already made the new destination visible and
                # moved the old destination to the stage path. Swap them back
                # before cleanup, otherwise cleanup_paths would delete the old
                # install as if it were temporary scratch.
                rollback_exchange=0
                if [[ -e "$dest" && -e "$stage" ]]; then
                    if mv -T --no-copy --exchange "$stage" "$dest"; then
                        if remove_safe_path "$stage"; then
                            rollback_exchange=1
                        else
                            ROLLBACK_FAILED=1
                        fi
                    else
                        ROLLBACK_FAILED=1
                    fi
                else
                    ROLLBACK_FAILED=1
                fi
                if (( rollback_exchange )); then
                    restore_prior_backup "$backup" "$prior" || true
                fi
                ;;
            published)
                rollback_published_exchange=0
                if [[ -e "$dest" && -e "$backup" ]]; then
                    if mv -T --no-copy --exchange "$backup" "$dest"; then
                        if remove_safe_path "$backup"; then
                            rollback_published_exchange=1
                        else
                            ROLLBACK_FAILED=1
                        fi
                    else
                        ROLLBACK_FAILED=1
                    fi
                else
                    ROLLBACK_FAILED=1
                fi
                # If the exchange failed, $backup still contains the old
                # destination. restore_prior_backup would delete that path
                # before moving the older .prev reservation into place.
                # Restore the prior only once the current backup is known to
                # have been exchanged away and removed successfully.
                if (( rollback_published_exchange )); then
                    restore_prior_backup "$backup" "$prior" || true
                fi
                ;;
            *)
                say "Rollback encountered unknown publish state '$state' for $dest; preserving paths for manual recovery." >&2
                ROLLBACK_FAILED=1
                ;;
        esac
    done
    ROLLBACK_NEEDED=0
}

cleanup_paths() {
    local index path

    for (( index=${#CLEANUP_PATHS[@]} - 1; index>=0; index-- )); do
        path="${CLEANUP_PATHS[$index]}"
        remove_safe_path "$path"
    done
    CLEANUP_PATHS=()
}

finish() {
    local status="$1"

    if (( status != 0 )) && (( COMMIT_COMPLETE == 0 )); then
        rollback_changes || true
    fi
    # If a rollback rename itself failed, do not let the generic scratch
    # cleanup remove a path whose ownership is now ambiguous. Leaving a
    # recoverable stage/prior path is safer than deleting the previous install.
    if (( ROLLBACK_FAILED == 0 )); then
        cleanup_paths || true
    else
        say "Rollback was incomplete; staged paths were preserved for manual recovery." >&2
    fi
    exit "$status"
}

trap 'finish "$?"' EXIT

use_systemd_run() {
    [[ "${PHOENIX_INSTALL_NO_SYSTEMD_RUN:-0}" != "1" ]] || return 1
    command -v systemd-run >/dev/null 2>&1 || return 1
    command -v systemctl >/dev/null 2>&1 || return 1
    [[ -n "${XDG_RUNTIME_DIR:-}" ]] || return 1
    [[ -z "${INVOCATION_ID:-}" ]] || return 1
    systemctl --user show-environment >/dev/null 2>&1
}

run_build() {
    local -a cargo_cmd=(cargo build --locked --release --bin phoenix --jobs "$CARGO_JOBS" --manifest-path "$BUILD_MANIFEST")

    if use_systemd_run; then
        say "Building Phoenix release binary (jobs=$CARGO_JOBS, runner=systemd-run --user --scope)..."
        systemd-run --user --scope --collect --quiet \
            -p MemoryHigh=6G -p MemoryMax=8G \
            nice -n 10 "${cargo_cmd[@]}"
    else
        say "Building Phoenix release binary (jobs=$CARGO_JOBS, runner=direct)..."
        nice -n 10 "${cargo_cmd[@]}"
    fi
}

validate_design_runtime() {
    local design_entry

    abs_path "$DESIGN_SRC" >/dev/null
    validate_source_tree "$DESIGN_SRC" "Iris design runtime"
    for design_entry in iris-design-runtime.mjs iris-design-preview.mjs; do
        abs_path "$SCRIPT_DIR/scripts/$design_entry" >/dev/null
        validate_regular_source_file "$SCRIPT_DIR/scripts/$design_entry" "Iris design entry point"
    done
    command -v "$DESIGN_NODE" >/dev/null 2>&1 || die "Iris design runtime requires Node.js 22 or newer (set PHOENIX_NODE to its executable)"
    "$DESIGN_NODE" -e 'if(Number(process.versions.node.split(".")[0])<22)process.exit(1)' || die "Iris design runtime requires Node.js 22 or newer"
    printf '%s\n' '{"action":"check","full":true}' | "$DESIGN_NODE" "$SCRIPT_DIR/scripts/iris-design-runtime.mjs" >/dev/null \
        || die "Iris design package integrity check failed — not installing"
}

stage_design_runtime() {
    local design_entry

    DESIGN_STAGE="$(make_stage_dir "$DESIGN_DEST" 0700)"
    # make_stage_dir runs in a command substitution; keep cleanup ownership in
    # this shell too, including when copying or staged validation fails.
    register_cleanup "$DESIGN_STAGE"
    mkdir -m 0700 "$DESIGN_STAGE/scripts" "$DESIGN_STAGE/vendor"
    cp -a --no-dereference "$DESIGN_SRC" "$DESIGN_STAGE/vendor/tastecode-design"
    for design_entry in iris-design-runtime.mjs iris-design-preview.mjs; do
        install -m 0644 "$SCRIPT_DIR/scripts/$design_entry" "$DESIGN_STAGE/scripts/$design_entry"
    done
    printf '%s\n' '{"action":"check","full":true}' | "$DESIGN_NODE" "$DESIGN_STAGE/scripts/iris-design-runtime.mjs" >/dev/null \
        || die "Staged Iris design package failed verification"
}

while (($#)); do
    case "$1" in
        -j|--jobs)
            shift
            (($#)) || die "missing value for --jobs"
            CARGO_JOBS="$1"
            JOBS_WAS_SET=1
            ;;
        --design-runtime-only)
            DESIGN_RUNTIME_ONLY=1
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            break
            ;;
        -*)
            die "unknown option: $1"
            ;;
        *)
            (( TARGET_WAS_SET == 0 )) || die "target directory provided more than once"
            TARGET="$1"
            TARGET_WAS_SET=1
            ;;
    esac
    shift
done

(($# == 0)) || die "unexpected trailing arguments"
if (( DESIGN_RUNTIME_ONLY )); then
    (( TARGET_WAS_SET == 0 && JOBS_WAS_SET == 0 )) \
        || die "--design-runtime-only cannot be combined with --jobs or target_dir; use PHOENIX_HOME for its state root"
else
    is_positive_integer "$CARGO_JOBS" || die "--jobs must be a positive integer"
fi

STATE_ROOT="$(resolve_state_root)"
PROMPTS_DEST="$STATE_ROOT/prompts"
TEMPLATES_DEST="$STATE_ROOT/templates"
DESIGN_DEST="$STATE_ROOT/design-runtime"

if (( DESIGN_RUNTIME_ONLY )); then
    validate_design_runtime
    DESIGN_DEST="$(ensure_safe_dir_slot "$DESIGN_DEST" 0700)"
    stage_design_runtime
    publish_staged_path "Iris design engine installed to $DESIGN_DEST (previous set at $(backup_path_for "$DESIGN_DEST"))" \
        "$DESIGN_STAGE" "$DESIGN_DEST" dir
    COMMIT_COMPLETE=1
    ROLLBACK_NEEDED=0
    cleanup_paths
    exit 0
fi

TARGET="$(ensure_safe_dir_path "$TARGET" 0755)"
BIN_DEST="$(ensure_safe_file_slot "$TARGET/phoenix" 0755)"

PROMPTS_ENABLED=0
if [[ -d "$PROMPTS_SRC" && -d "$PROMPTS_DEST" ]]; then
    PROMPTS_ENABLED=1
    PROMPTS_DEST="$(ensure_safe_dir_slot "$PROMPTS_DEST" 0700)"
fi

TEMPLATES_ENABLED=0
if [[ -d "$TEMPLATES_SRC" ]]; then
    TEMPLATES_ENABLED=1
    TEMPLATES_DEST="$(ensure_safe_dir_slot "$TEMPLATES_DEST" 0700)"
fi

DESIGN_ENABLED=0
if [[ -d "$DESIGN_SRC" ]]; then
    DESIGN_ENABLED=1
    DESIGN_DEST="$(ensure_safe_dir_slot "$DESIGN_DEST" 0700)"
    validate_design_runtime
fi

EXT_ENABLED=0
if [[ -d "$EXT_SRC" && -n "${WAYLAND_DISPLAY:-}" ]] && command -v gnome-extensions >/dev/null 2>&1; then
    EXT_ENABLED=1
    EXT_DEST="$(ensure_safe_dir_slot "$EXT_DEST" 0755)"
fi

validate_regular_source_file "$BUILD_MANIFEST" "Cargo manifest"
run_build || die "build failed — not installing"
validate_regular_source_file "$BUILD_ARTIFACT" "built Phoenix binary"

BIN_STAGE="$(make_stage_file "$BIN_DEST")"
install -m 0755 "$BUILD_ARTIFACT" "$BIN_STAGE"

if (( PROMPTS_ENABLED )); then
    prompt_files=( "$PROMPTS_SRC"/*_system.md )
    ((${#prompt_files[@]} > 0)) || die "no prompt overlay source files matched $PROMPTS_SRC/*_system.md"
    PROMPTS_STAGE="$(make_stage_dir "$PROMPTS_DEST" 0700)"
    cp -a --no-dereference "$PROMPTS_DEST/." "$PROMPTS_STAGE/"
    for prompt_file in "${prompt_files[@]}"; do
        validate_regular_source_file "$prompt_file" "prompt overlay"
        install -m 0644 "$prompt_file" "$PROMPTS_STAGE/$(basename "$prompt_file")"
    done
    for stale_prompt in "$PROMPTS_STAGE"/*_system.md; do
        base="$(basename "$stale_prompt")"
        [[ -f "$PROMPTS_SRC/$base" ]] || rm -f -- "$stale_prompt"
    done
fi

if (( TEMPLATES_ENABLED )); then
    validate_source_tree "$TEMPLATES_SRC" "template"
    TEMPLATES_STAGE="$(make_stage_dir "$TEMPLATES_DEST" 0700)"
    cp -a --no-dereference "$TEMPLATES_SRC/." "$TEMPLATES_STAGE/"
fi

if (( DESIGN_ENABLED )); then
    stage_design_runtime
fi

if (( EXT_ENABLED )); then
    validate_source_tree "$EXT_SRC" "GNOME extension"
    EXT_STAGE="$(make_stage_dir "$EXT_DEST" 0755)"
    cp -a --no-dereference "$EXT_SRC/." "$EXT_STAGE/"
fi

if (( PROMPTS_ENABLED )); then
    publish_staged_path "Prompt overlays synced to $PROMPTS_DEST (previous set at $(backup_path_for "$PROMPTS_DEST"))" \
        "$PROMPTS_STAGE" "$PROMPTS_DEST" dir
fi

if (( TEMPLATES_ENABLED )); then
    publish_staged_path "Templates synced to $TEMPLATES_DEST (previous set at $(backup_path_for "$TEMPLATES_DEST"))" \
        "$TEMPLATES_STAGE" "$TEMPLATES_DEST" dir
fi

if (( EXT_ENABLED )); then
    publish_staged_path "Phoenix cursor extension installed to $EXT_DEST (previous set at $(backup_path_for "$EXT_DEST"))" \
        "$EXT_STAGE" "$EXT_DEST" dir
fi

if (( DESIGN_ENABLED )); then
    publish_staged_path "Iris design engine installed to $DESIGN_DEST (previous set at $(backup_path_for "$DESIGN_DEST"))" \
        "$DESIGN_STAGE" "$DESIGN_DEST" dir
fi

publish_staged_path "Installed to $BIN_DEST (previous binary at $(backup_path_for "$BIN_DEST"))" \
    "$BIN_STAGE" "$BIN_DEST" file
say "  (Browser agent drives native Chromium over CDP — needs a local Chrome/Chromium.)"

if (( EXT_ENABLED )); then
    if gnome-extensions info "$EXT_UUID" 2>/dev/null | grep -q "Enabled: Yes"; then
        say "  Extension already enabled."
    else
        say "  ONE-TIME: log out and back in, then run:"
        say "    gnome-extensions enable $EXT_UUID"
    fi
fi

COMMIT_COMPLETE=1
ROLLBACK_NEEDED=0
cleanup_paths

say
say "Add $TARGET to your PATH if needed:"
say '  export PATH="$HOME/.local/bin:$PATH"'
say
say "Then run:"
say "  phoenix onboard    # First time setup"
say "  phoenix start      # Run the agent"
say "  phoenix check      # Verify provider connectivity"
