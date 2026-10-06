//! design_reference — embedded design-skill libraries.
//!
//! Three libraries compiled into the binary, so they are available
//! regardless of workspace, install layout, or permission mode:
//!
//! - **hallmark** (MIT) — the general design system: genres, macrostructures,
//!   component archetypes, slop test. Default paths (`SKILL.md`,
//!   `references/...`) resolve here. Consumed the way it was designed: slim
//!   index first, then ONLY the picked files, slop-test last.
//! - **Taste** — Phoenix's mandatory primary contract for every visual-design
//!   task: brief inference ("design read"), three tunable dials (variance /
//!   motion / density), bias-correction directives, and craft gates. One
//!   self-contained file at `taste/SKILL.md`; the other libraries supplement
//!   it for a particular artifact or stack, never replace it.
//! - **the-10k-look** (Fable, 2026-07-07) — field guide for 3D / immersive /
//!   award-style sites: ten laws of the expensive feel, the stack
//!   (three.js/R3F, GSAP, Lenis), the motion language (damped values, easing,
//!   choreography), lighting/materials, shaders, reactive-background recipes,
//!   postprocessing budgets, scroll craft, performance, a11y floor, pre-ship
//!   checklist. One self-contained file at `10k-look/SKILL.md` (alias `10k`).
//! - **slides** (stackblitz/bolt-slides, MIT — 2026-07-16) — the presented-deck
//!   engine: paged Vite+React slides with click-builds, presenter mode, and a
//!   ~25-component slide library. The authoring guide lives at
//!   `slides/SKILL.md` (alias `bolt-slides`); the runnable template it
//!   describes is synced to `~/.phoenix/templates/slides` by install.sh.

use anyhow::{anyhow, Result};
use include_dir::{include_dir, Dir};
use serde::Deserialize;

use super::ToolOutput;

static LIBRARY: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/prompts/frontend/hallmark");
static TASTE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/prompts/frontend/taste");
static TENK: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/prompts/frontend/10k-look");
static SLIDES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/prompts/frontend/slides");
static STUDIO: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/prompts/frontend/frontend-design");
static HIGH_AGENCY: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/prompts/frontend/design-taste");
static KOMBAI: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/prompts/frontend/kombai");
static VIBECURB: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/prompts/frontend/vibecurb");
static VISUAL: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/prompts/frontend/visual");

/// Single-file libraries riding at their own path prefixes. A bare prefix
/// means that library's SKILL.md. Aliases map to the same dir.
/// Phoenix's own vocabulary for its design libraries.
///
/// The FIRST prefix registered for a Dir is canonical, so the Phoenix name is
/// what every agent sees and pins. The donor names that follow each one are
/// kept ONLY as silent aliases: prompts, memory notes and saved sessions out
/// there still reference them, and breaking those references to win a rename
/// would be a worse trade than carrying twelve dead strings. They are absent
/// from the index text below, which is the surface agents actually read.
const SINGLE_FILE_LIBS: &[(&str, &Dir<'_>)] = &[
    ("taste", &TASTE),
    ("craft", &TASTE),
    ("immersive", &TENK),
    ("10k-look", &TENK),
    ("10k", &TENK),
    ("slides", &SLIDES),
    ("bolt-slides", &SLIDES),
    ("studio", &STUDIO),
    ("frontend-design", &STUDIO),
    ("high-agency", &HIGH_AGENCY),
    ("design-taste", &HIGH_AGENCY),
    ("process", &KOMBAI),
    ("kombai", &KOMBAI),
    ("strict", &VIBECURB),
    ("vibecurb", &VIBECURB),
    ("curb", &VIBECURB),
    ("visual", &VISUAL),
    ("field-guide", &VISUAL),
];

#[derive(Debug, Deserialize)]
pub struct DesignReferenceInput {
    /// Library-relative path, e.g. `SKILL.md` or
    /// `references/macrostructures/05-workbench.md`. Omit to list the index.
    #[serde(default)]
    pub path: Option<String>,
}

/// Collapse alias prefixes (`10k` → `10k-look`, `curb` → `vibecurb`,
/// `frontend-design` → `studio`, …) so a library loaded under two names pins
/// once in the session's reference registry. The FIRST registered prefix for a
/// Dir is canonical. Unknown paths pass through unchanged (Hallmark files).
pub fn canonical_path(path: &str) -> String {
    let normalized = path.trim().trim_start_matches("./").trim_start_matches('/');
    for (prefix, lib) in SINGLE_FILE_LIBS {
        let Some(rest) = normalized
            .strip_prefix(prefix)
            .filter(|r| r.is_empty() || r.starts_with('/'))
        else {
            continue;
        };
        let canonical = SINGLE_FILE_LIBS
            .iter()
            .find(|(_, l)| std::ptr::eq::<Dir<'_>>(*l, *lib))
            .map(|(p, _)| *p)
            .unwrap_or(*prefix);
        let inner = rest.trim_start_matches('/');
        let inner = if inner.is_empty() { "SKILL.md" } else { inner };
        return format!("{canonical}/{inner}");
    }
    normalized.to_string()
}

pub fn execute(input: DesignReferenceInput) -> Result<ToolOutput> {
    let (summary, content) = match input.path.as_deref().map(str::trim) {
        None | Some("") | Some("index") => ("design library index".to_string(), index()),
        Some(path) => {
            let normalized = path.trim_start_matches("./").trim_start_matches('/');
            // `taste` / `taste/...` / `10k-look` / `10k` route to the
            // single-file libraries (a bare prefix means that SKILL.md).
            for (prefix, lib) in SINGLE_FILE_LIBS {
                let Some(rest) = normalized
                    .strip_prefix(prefix)
                    .filter(|r| r.is_empty() || r.starts_with('/'))
                else {
                    continue;
                };
                let inner = rest.trim_start_matches('/');
                let inner = if inner.is_empty() { "SKILL.md" } else { inner };
                // Report the CANONICAL Phoenix name, never the alias the caller
                // happened to use. Echoing `{prefix}` back taught the donor name
                // to any agent that arrived through an alias — the exact leak
                // the canonical table exists to close.
                let canon = SINGLE_FILE_LIBS
                    .iter()
                    .find(|(_, l)| std::ptr::eq::<Dir<'_>>(*l, *lib))
                    .map(|(p, _)| *p)
                    .unwrap_or(*prefix);
                // Taste's source handbook intentionally carries long examples,
                // appendices, and citation material. Injecting all 88KB on
                // every round—and once more in the load receipt—crowded the
                // actual UI brief out of the model's attention. The canonical
                // SKILL path now resolves to a reviewed bounded execution
                // contract; the full source remains embedded and addressable
                // as `taste/source.md` for deliberate reference inspection.
                let resolved_inner = if std::ptr::eq::<Dir<'_>>(*lib, &TASTE) && inner == "SKILL.md"
                {
                    "RUNTIME.md"
                } else if std::ptr::eq::<Dir<'_>>(*lib, &TASTE) && inner == "source.md" {
                    "SKILL.md"
                } else {
                    inner
                };
                let file = lib.get_file(resolved_inner).ok_or_else(|| {
                    anyhow!(
                        "no design reference at `{canon}/{inner}`. This library is one file: `{canon}/SKILL.md`."
                    )
                })?;
                let content = file
                    .contents_utf8()
                    .ok_or_else(|| anyhow!("design reference `{path}` is not valid UTF-8"))?
                    .to_string();
                return Ok(ToolOutput {
                    summary: format!("design reference {canon}/{inner}"),
                    content,
                });
            }
            let file = LIBRARY
                .get_file(normalized)
                .or_else(|| LIBRARY.get_file(format!("{normalized}.md")))
                .or_else(|| resolve_by_prefix(normalized));
            let Some(file) = file else {
                return Err(anyhow!(miss_message(normalized)));
            };
            let resolved = file.path().to_string_lossy().to_string();
            let content = file
                .contents_utf8()
                .ok_or_else(|| anyhow!("design reference `{path}` is not valid UTF-8"))?
                .to_string();
            (format!("design reference {resolved}"), content)
        }
    };
    Ok(ToolOutput { summary, content })
}

/// Resolve a near-miss path by archetype code / slug prefix. Models reliably
/// pick the right CODE from the slim indexes ("f1", "n9", "05") but guess the
/// slug ("f1-feature-grid" for the real "f1-bento-grid"), so match on the
/// hyphen-delimited prefix of the basename within the same directory. A bare
/// code ("f1", "components/f1") searches the components and macrostructures
/// directories.
fn resolve_by_prefix(normalized: &str) -> Option<&'static include_dir::File<'static>> {
    let (dir_part, base) = match normalized.rsplit_once('/') {
        Some((d, b)) => (d, b),
        None => ("", normalized),
    };
    let code = base
        .trim_end_matches(".md")
        .split('-')
        .next()?
        .to_ascii_lowercase();
    if code.is_empty() {
        return None;
    }
    let dirs: Vec<&Dir> = if dir_part.is_empty() {
        ["references/components", "references/macrostructures"]
            .iter()
            .filter_map(|d| LIBRARY.get_dir(d))
            .collect()
    } else {
        LIBRARY.get_dir(dir_part).into_iter().collect()
    };
    for dir in dirs {
        for file in dir.files() {
            let name = file.path().file_name()?.to_string_lossy().to_lowercase();
            // `f1-...` must match code `f1` but not `ft1`/`f10`.
            if name
                .strip_prefix(&code)
                .is_some_and(|rest| rest.starts_with('-') || rest == ".md" || rest.starts_with('.'))
            {
                return Some(file);
            }
        }
    }
    None
}

/// Self-healing miss: tell the model what IS there, scoped to the directory it
/// aimed at, so the repair round needs zero extra calls.
fn miss_message(normalized: &str) -> String {
    let dir_part = normalized.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    if let Some(dir) = LIBRARY.get_dir(dir_part) {
        let siblings: Vec<String> = dir
            .files()
            .filter_map(|f| {
                f.path()
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .collect();
        format!(
            "no design reference at `{normalized}`. Files in `{dir_part}/`: {}",
            siblings.join(", ")
        )
    } else {
        let mut paths = Vec::new();
        collect_paths(&LIBRARY, &mut paths);
        paths.sort();
        format!(
            "no design reference at `{normalized}` (directory doesn't exist either). Valid paths:\n{}",
            paths.join("\n")
        )
    }
}

/// One line per file, grouped the way the skill's load discipline groups them.
fn index() -> String {
    let mut paths = Vec::new();
    collect_paths(&LIBRARY, &mut paths);
    paths.sort();
    let mut out = String::from(
        "Phoenix design references. For an actual visual build, load taste/SKILL.md first.\n\
         It is the bounded integrated brief → brand → page → assets → build → review contract.\n\
         Read-only conversation and research do not require loading a design guide.\n\
         design_studio supplies local palette, typography, blueprint and copy checks; actual browser/vision review remains necessary.\n\
         Load AT MOST ONE exact supplement only for a genuinely missing workflow. None overrides the user or primary contract.\n\n\
         Primary: taste/SKILL.md. Optional native motion implementation: taste/reveal.js.\n\
         Full historical source (deliberate reference only): taste/source.md.\n\
         Visual craft (3D, spatial, SVG/avatars, web layout, references, judging renders): visual/SKILL.md indexes visual/looking.md, visual/3d.md, visual/svg.md, visual/web.md, visual/references.md.\n\
         Task-specific: studio/SKILL.md; process/SKILL.md; high-agency/SKILL.md; immersive/SKILL.md; slides/SKILL.md.\n\
         Strict pipelines: strict/SKILL.md, strict/pixel-perfect.md, strict/visual-redesign.md, strict/hero.md, strict/motion.md, strict/imagegen.md.\n\
         For product UI, select only the needed system, genre, macrostructure or component reference below.\n\
         Deck template: ~/.phoenix/templates/slides.\n\
         Source-library identities and legal notices are not product branding; never reproduce them in the generated UI.\n\nSupporting reference files:\n",
    );
    for path in paths { out.push_str("  "); out.push_str(&path); out.push('\n'); }
    out
}

fn collect_paths(dir: &Dir<'_>, out: &mut Vec<String>) {
    for file in dir.files() {
        let path = file.path().to_string_lossy().to_string();
        if path.ends_with(".md") {
            out.push(path);
        }
    }
    for sub in dir.dirs() {
        collect_paths(sub, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(output: ToolOutput) -> String {
        output.content
    }

    #[test]
    fn index_lists_skill_and_references() {
        let out = text(execute(DesignReferenceInput { path: None }).unwrap());
        assert!(out.contains("SKILL.md"));
        assert!(out.contains("references/slop-test.md"));
        assert!(out.contains("references/macrostructures/"));
        assert!(out.contains("references/components/"));
        // The single-file libraries ride along in the same index.
        assert!(out.contains("taste/SKILL.md"));
        assert!(out.contains("immersive/SKILL.md"));
    }

    #[test]
    fn taste_library_loads_by_prefix_or_bare_name() {
        for path in ["craft/SKILL.md", "taste"] {
            let out = text(
                execute(DesignReferenceInput {
                    path: Some(path.into()),
                })
                .unwrap(),
            );
            assert!(out.contains("DESIGN_VARIANCE"), "taste dials missing");
        }
        // A miss inside taste names the one valid file, and `tastey` is not taste.
        let err = execute(DesignReferenceInput {
            path: Some("taste/references/nope.md".into()),
        })
        .unwrap_err();
        assert!(err.to_string().contains("taste/SKILL.md"));
        assert!(execute(DesignReferenceInput {
            path: Some("tastey".into()),
        })
        .is_err());
    }

    #[test]
    fn taste_runtime_is_bounded_while_the_full_handbook_remains_addressable() {
        let runtime = text(
            execute(DesignReferenceInput {
                path: Some("taste/SKILL.md".into()),
            })
            .unwrap(),
        );
        let source = text(
            execute(DesignReferenceInput {
                path: Some("taste/source.md".into()),
            })
            .unwrap(),
        );
        assert!(
            runtime.chars().count() < 8_000,
            "runtime grew to {} chars",
            runtime.chars().count()
        );
        assert!(runtime.contains("build or edit that component"));
        assert!(source.chars().count() > 80_000);
        assert!(source.contains("APPENDICES - Real Source-Backed Reference Material"));
    }

    #[test]
    fn visual_field_guide_loads_index_and_each_topic() {
        let load = |path: &str| text(execute(DesignReferenceInput { path: Some(path.into()) }).unwrap());
        assert!(load("visual").contains("Visual field guide"));
        assert!(load("field-guide/SKILL.md").contains("The seven laws"));
        for (path, marker) in [
            ("visual/looking.md", "Squint test"),
            ("visual/3d.md", "Spatial reasoning"),
            ("visual/svg.md", "Characters, avatars and faces"),
            ("visual/web.md", "Pick the layout from the job"),
            ("visual/references.md", "Look at each candidate once"),
        ] {
            assert!(load(path).contains(marker), "{path}");
        }
        assert!(index().contains("visual/SKILL.md"));
        // General craft only: the guide must never encode a test subject.
        for file in VISUAL.files() {
            let text = file.contents_utf8().unwrap_or_default().to_ascii_lowercase();
            assert!(!text.contains("banana"), "{:?} names a specific test subject", file.path());
        }
    }

    #[test]
    fn tenk_library_loads_by_full_name_alias_or_explicit_path() {
        for path in ["10k-look", "10k", "immersive/SKILL.md", "10k/SKILL.md"] {
            let out = text(
                execute(DesignReferenceInput {
                    path: Some(path.into()),
                })
                .unwrap(),
            );
            assert!(out.contains("THE $10K LOOK"), "guide missing via `{path}`");
        }
        // A miss inside the library names its one valid file.
        let err = execute(DesignReferenceInput {
            path: Some("10k-look/references/nope.md".into()),
        })
        .unwrap_err();
        assert!(err.to_string().contains("immersive/SKILL.md"));
    }

    #[test]
    fn vibecurb_router_and_pipelines_load() {
        // Bare prefix (and alias) → the short router index.
        for path in ["vibecurb", "curb", "strict/SKILL.md"] {
            let out = text(
                execute(DesignReferenceInput {
                    path: Some(path.into()),
                })
                .unwrap(),
            );
            assert!(out.contains("LOAD DISCIPLINE"), "router via `{path}`");
        }
        // Each pipeline file loads whole by its inner path.
        for f in [
            "pixel-perfect.md",
            "visual-redesign.md",
            "hero.md",
            "motion.md",
            "imagegen.md",
        ] {
            let out = text(
                execute(DesignReferenceInput {
                    path: Some(format!("strict/{f}")),
                })
                .unwrap(),
            );
            assert!(out.len() > 10_000, "strict/{f} loads whole");
        }
        let idx = text(execute(DesignReferenceInput { path: None }).unwrap());
        assert!(
            idx.contains("strict/pixel-perfect.md"),
            "index lists vibecurb"
        );
    }

    #[test]
    fn reads_skill_md() {
        let out = text(
            execute(DesignReferenceInput {
                path: Some("SKILL.md".into()),
            })
            .unwrap(),
        );
        assert!(out.contains("macrostructure"));
    }

    #[test]
    fn reads_nested_reference_with_or_without_extension() {
        for path in ["references/typography.md", "references/typography"] {
            let out = text(
                execute(DesignReferenceInput {
                    path: Some(path.into()),
                })
                .unwrap(),
            );
            assert!(!out.is_empty());
        }
    }

    #[test]
    fn unknown_path_error_lists_valid_siblings() {
        let err = execute(DesignReferenceInput {
            path: Some("references/nope.md".into()),
        })
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("slop-test.md"), "{msg}");
    }

    #[test]
    fn wrong_slug_right_code_resolves_by_prefix() {
        // The live failure: model guessed `f1-feature-grid.md`; real file is
        // `f1-bento-grid.md`. Code prefix must win; `f1` must not match `ft1`.
        let out = text(
            execute(DesignReferenceInput {
                path: Some("references/components/f1-feature-grid.md".into()),
            })
            .unwrap(),
        );
        assert!(!out.is_empty());
        let summary = execute(DesignReferenceInput {
            path: Some("references/components/f1-whatever".into()),
        })
        .unwrap()
        .summary;
        assert!(summary.contains("f1-bento-grid.md"), "{summary}");
        assert!(!summary.contains("ft1"), "{summary}");
    }

    #[test]
    fn bare_archetype_code_resolves() {
        let summary = execute(DesignReferenceInput {
            path: Some("n9".into()),
        })
        .unwrap()
        .summary;
        assert!(summary.contains("n9-edge-aligned-minimal.md"), "{summary}");
    }

    #[test]
    fn nonexistent_genre_lists_real_genres() {
        // Live failure: model guessed `references/genres/terminal.md`.
        let err = execute(DesignReferenceInput {
            path: Some("references/genres/terminal.md".into()),
        })
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("editorial.md"), "{msg}");
    }

    #[test]
    fn the_slides_skill_names_a_template_path_that_matches_install_sh() {
        // The rename that broke this: the PROMPT was updated to
        // ~/.phoenix/templates/slides while the directory on disk stayed
        // bolt-slides, so the agent was told to copy an engine that was not
        // there and hand-rolled a deck instead. Nothing failed loudly — the
        // path simply did not exist. Pin the string the skill actually ships.
        let skill = text(
            execute(DesignReferenceInput {
                path: Some("slides/SKILL.md".into()),
            })
            .unwrap(),
        );
        assert!(
            skill.contains("~/.phoenix/templates/slides"),
            "the slides skill must point at the installed template dir"
        );
        assert!(
            !skill.contains("templates/bolt-slides"),
            "stale donor template path still in the skill body"
        );
    }
}
