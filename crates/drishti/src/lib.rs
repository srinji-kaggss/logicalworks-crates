//! # Drishti: frontend is orchestration
//!
//! **0.0.1 is a documentation-only design publication, not a frontend runtime.**
//! These modules contain substantive architecture documentation. There are no
//! renderer, scheduler, sandbox, scripting, or application runtime APIs here.
//!
//! Start with [vision], then [architecture], [codebase], [decisions], [research],
//! and [assurance]. [contracts] contains the requirement and journey registries.
//! [status] separates package evidence from unimplemented product obligations.
//!
//! The Rustdoc modules above are the design publication. The first executable
//! slice of the presentation layer lives in [design] (Logical Works brand
//! tokens), [ink_lab] (the liquid ink engine) and [runtime] (the GPU residency
//! model). These are deterministic, dependency-free reference cores with unit
//! and simulation tests; they are not yet qualified on a real GPU or host, and
//! no product journey is claimed passed because of them. The `drishti` command
//! (`src/bin/drishti.rs`) is their consumer: it prints the tokens, the ink
//! frames, a residency session and the limits as JSON, and each page below.

pub mod design;
pub mod ink_lab;
pub mod runtime;

/// The native GPU host (GPUI through `lgwks_deps`), behind the `ui` feature.
#[cfg(feature = "ui")]
pub mod native;

#[doc = include_str!("../VISION.md")]
pub mod vision {
    /// This page's Markdown, as `drishti docs vision` prints it.
    pub const SOURCE: &str = include_str!("../VISION.md");
}

#[doc = concat!(
    include_str!("../docs/architecture.md"),
    "\n\n",
    include_str!("../docs/architecture-operations.md")
)]
pub mod architecture {
    /// This page's Markdown, as `drishti docs architecture` prints it.
    pub const SOURCE: &str = concat!(
        include_str!("../docs/architecture.md"),
        "\n\n",
        include_str!("../docs/architecture-operations.md")
    );
}

#[doc = concat!(
    include_str!("../docs/codebase.md"),
    "\n\n",
    include_str!("../docs/planned-tree.md")
)]
pub mod codebase {
    /// This page's Markdown, as `drishti docs codebase` prints it.
    pub const SOURCE: &str = concat!(
        include_str!("../docs/codebase.md"),
        "\n\n",
        include_str!("../docs/planned-tree.md")
    );
}

#[doc = concat!(
    include_str!("../docs/decisions.md"),
    "\n\n",
    include_str!("../docs/contract.md")
)]
pub mod decisions {
    /// This page's Markdown, as `drishti docs decisions` prints it.
    pub const SOURCE: &str = concat!(
        include_str!("../docs/decisions.md"),
        "\n\n",
        include_str!("../docs/contract.md")
    );
}

#[doc = include_str!("../docs/research.md")]
pub mod research {
    /// This page's Markdown, as `drishti docs research` prints it.
    pub const SOURCE: &str = include_str!("../docs/research.md");
}

#[doc = include_str!("../docs/assurance.md")]
pub mod assurance {
    /// This page's Markdown, as `drishti docs assurance` prints it.
    pub const SOURCE: &str = include_str!("../docs/assurance.md");
}

#[doc = concat!(
    include_str!("../docs/status.md"),
    "\n\n",
    include_str!("../docs/status-history.md")
)]
pub mod status {
    /// This page's Markdown, as `drishti docs status` prints it.
    pub const SOURCE: &str = concat!(
        include_str!("../docs/status.md"),
        "\n\n",
        include_str!("../docs/status-history.md")
    );
}

#[doc = concat!(
    include_str!("../docs/inklab.md"),
    "\n\n",
    include_str!("../docs/ink-engine.md")
)]
pub mod inklab_documentation {
    /// This page's Markdown, as `drishti docs inklab` prints it.
    pub const SOURCE: &str = concat!(
        include_str!("../docs/inklab.md"),
        "\n\n",
        include_str!("../docs/ink-engine.md")
    );
}

#[doc = concat!(
    include_str!("../docs/gpu-runtime.md"),
    "\n\n",
    include_str!("../docs/native-host.md")
)]
pub mod gpu_runtime_documentation {
    /// This page's Markdown, as `drishti docs gpu-runtime` prints it.
    pub const SOURCE: &str = concat!(
        include_str!("../docs/gpu-runtime.md"),
        "\n\n",
        include_str!("../docs/native-host.md")
    );
}

#[doc = concat!(
    "# Requirements and unexecuted acceptance journeys\n\n",
    "These tab-separated registries specify obligations, not observed behavior.\n\n",
    "## Requirements\n\n```text\n",
    include_str!("../contract/requirements.tsv"),
    "\n```\n\n## Journeys\n\n```text\n",
    include_str!("../experience/journeys.tsv"),
    "\n```\n"
)]
pub mod contracts {
    /// The two registries, as `drishti docs contracts` prints them.
    pub const SOURCE: &str = concat!(
        include_str!("../contract/requirements.tsv"),
        "\n",
        include_str!("../experience/journeys.tsv")
    );
}
