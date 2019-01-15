//! Provides the `Session` type, which represents the user's state during an
//! execution of a Notion tool, including their current directory, Notion
//! hooks, and the state of the local inventory.

use std::rc::Rc;

use distro::node::NodeVersion;
use distro::Fetched;
use hook::Publish;
use hook::{Hooks, LazyHooks};
use inventory::{Inventory, LazyInventory};
use platform::PlatformSpec;
use project::Project;
use toolchain::Toolchain;
use version::VersionSpec;

use std::fmt::{self, Display, Formatter};
use std::process::exit;

use event::EventLog;
use notion_fail::{ExitCode, Fallible, NotionError, NotionFail};
use semver::Version;

#[derive(Eq, PartialEq, Ord, PartialOrd, Clone, Copy)]
pub enum ActivityKind {
    Fetch,
    Install,
    Uninstall,
    Current,
    Deactivate,
    Activate,
    Default,
    Pin,
    Node,
    Npm,
    Npx,
    Yarn,
    Notion,
    Tool,
    Help,
    Version,
    Binary,
    Shim,
}

impl Display for ActivityKind {
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        let s = match self {
            &ActivityKind::Fetch => "fetch",
            &ActivityKind::Install => "install",
            &ActivityKind::Uninstall => "uninstall",
            &ActivityKind::Current => "current",
            &ActivityKind::Deactivate => "deactivate",
            &ActivityKind::Activate => "activate",
            &ActivityKind::Default => "default",
            &ActivityKind::Pin => "pin",
            &ActivityKind::Node => "node",
            &ActivityKind::Npm => "npm",
            &ActivityKind::Npx => "npx",
            &ActivityKind::Yarn => "yarn",
            &ActivityKind::Notion => "notion",
            &ActivityKind::Tool => "tool",
            &ActivityKind::Help => "help",
            &ActivityKind::Version => "version",
            &ActivityKind::Binary => "binary",
            &ActivityKind::Shim => "shim",
        };
        f.write_str(s)
    }
}

/// Thrown when the user tries to pin Node or Yarn versions outside of a package.
#[derive(Debug, Fail, NotionFail)]
#[fail(display = "Not in a node package")]
#[notion_fail(code = "ConfigurationError")]
pub(crate) struct NotInPackageError;

impl NotInPackageError {
    pub(crate) fn new() -> Self {
        NotInPackageError
    }
}

/// Represents the user's state during an execution of a Notion tool. The session
/// encapsulates a number of aspects of the environment in which the tool was
/// invoked, including:
///     - the current directory
///     - the Node project tree that contains the current directory (if any)
///     - the Notion hook settings
///     - the inventory of locally-fetched Notion tools
pub struct Session {
    hooks: LazyHooks,
    inventory: LazyInventory,
    toolchain: Toolchain,
    project: Option<Rc<Project>>,
    event_log: EventLog,
}

impl Session {
    /// Constructs a new `Session`.
    pub fn new() -> Fallible<Session> {
        Ok(Session {
            hooks: LazyHooks::new(),
            inventory: LazyInventory::new(),
            toolchain: Toolchain::current()?,
            project: Project::for_current_dir()?.map(Rc::new),
            event_log: EventLog::new()?,
        })
    }

    /// Produces a reference to the current Node project, if any.
    pub fn project(&self) -> Option<Rc<Project>> {
        self.project.clone()
    }

    pub fn current_platform(&mut self) -> Fallible<Option<Rc<PlatformSpec>>> {
        if let Some(image) = self.project_platform() {
            return Ok(Some(image));
        }

        if let Some(image) = self.user_platform()? {
            return Ok(Some(image));
        }

        return Ok(None);
    }

    pub fn user_platform(&mut self) -> Fallible<Option<Rc<PlatformSpec>>> {
        if let Some(node) = self.user_node() {
            if let Some(yarn) = self.user_yarn() {
                return Ok(Some(Rc::new(PlatformSpec {
                    node,
                    yarn: Some(yarn),
                })));
            }

            return Ok(Some(Rc::new(PlatformSpec { node, yarn: None })));
        }
        Ok(None)
    }

    /// Returns the current project's pinned platform image, if any.
    pub fn project_platform(&self) -> Option<Rc<PlatformSpec>> {
        if let Some(ref project) = self.project {
            return project.platform();
        }
        None
    }

    /// Produces a reference to the current inventory.
    pub fn inventory(&self) -> Fallible<&Inventory> {
        self.inventory.get()
    }

    /// Produces a mutable reference to the current inventory.
    pub fn inventory_mut(&mut self) -> Fallible<&mut Inventory> {
        self.inventory.get_mut()
    }

    /// Produces a reference to the hooks.
    pub fn hooks(&self) -> Fallible<&Hooks> {
        self.hooks.get()
    }

    /// Ensures that a specific Node version has been fetched and unpacked
    pub(crate) fn ensure_node(&mut self, version: &Version) -> Fallible<()> {
        let inventory = self.inventory.get_mut()?;

        if !inventory.node.contains(version) {
            let hooks = self.hooks.get()?;
            inventory.fetch_node(&VersionSpec::exact(version), hooks)?;
        }

        Ok(())
    }

    /// Ensures that a specific Yarn version has been fetched and unpacked
    pub(crate) fn ensure_yarn(&mut self, version: &Version) -> Fallible<()> {
        let inventory = self.inventory.get_mut()?;

        if !inventory.yarn.contains(version) {
            let hooks = self.hooks.get()?;
            inventory.fetch_yarn(&VersionSpec::exact(version), hooks)?;
        }

        Ok(())
    }

    pub fn user_node(&self) -> Option<NodeVersion> {
        self.toolchain.get_active_node().map(|ref nv| nv.clone())
    }

    /// Fetches a version of Node matching the specified semantic verisoning
    /// requirements.
    pub fn fetch_node(&mut self, matching: &VersionSpec) -> Fallible<Fetched<NodeVersion>> {
        let inventory = self.inventory.get_mut()?;
        let hooks = self.hooks.get()?;
        inventory.fetch_node(matching, hooks)
    }

    /// Sets the user toolchain's Node version to one matching the specified semantic versioning
    /// requirements.
    pub fn install_node(&mut self, matching: &VersionSpec) -> Fallible<()> {
        let inventory = self.inventory.get_mut()?;
        let hooks = self.hooks.get()?;
        let version = inventory.fetch_node(matching, hooks)?.into_version();
        self.toolchain.set_active_node(version)?;
        Ok(())
    }

    /// Updates toolchain in package.json with the Node version matching the specified semantic
    /// versioning requirements.
    pub fn pin_node_version(&mut self, matching: &VersionSpec) -> Fallible<()> {
        if let Some(ref project) = self.project() {
            let node_version = self.fetch_node(matching)?.into_version();
            project.pin_node_in_toolchain(node_version)?;
        } else {
            throw!(NotInPackageError::new());
        }
        Ok(())
    }

    pub fn user_yarn(&mut self) -> Option<Version> {
        self.toolchain.get_active_yarn().map(|ref v| v.clone())
    }

    /// Fetches a version of Node matching the specified semantic verisoning
    /// requirements.
    pub fn fetch_yarn(&mut self, matching: &VersionSpec) -> Fallible<Fetched<Version>> {
        let inventory = self.inventory.get_mut()?;
        let hooks = self.hooks.get()?;
        inventory.fetch_yarn(matching, hooks)
    }

    /// Sets the Yarn version in the user toolchain to one matching the specified semantic versioning
    /// requirements.
    pub fn install_yarn(&mut self, matching: &VersionSpec) -> Fallible<()> {
        let inventory = self.inventory.get_mut()?;
        let hooks = self.hooks.get()?;
        let version = inventory.fetch_yarn(matching, hooks)?.into_version();
        self.toolchain.set_active_yarn(version)?;
        Ok(())
    }

    /// Updates toolchain in package.json with the Yarn version matching the specified semantic
    /// versioning requirements.
    pub fn pin_yarn_version(&mut self, matching: &VersionSpec) -> Fallible<()> {
        if let Some(ref project) = self.project() {
            let yarn_version = self.fetch_yarn(matching)?.into_version();
            project.pin_yarn_in_toolchain(yarn_version)?;
        } else {
            throw!(NotInPackageError::new());
        }
        Ok(())
    }

    pub fn add_event_start(&mut self, activity_kind: ActivityKind) {
        self.event_log.add_event_start(activity_kind)
    }
    pub fn add_event_end(&mut self, activity_kind: ActivityKind, exit_code: ExitCode) {
        self.event_log.add_event_end(activity_kind, exit_code)
    }
    pub fn add_event_tool_end(&mut self, activity_kind: ActivityKind, exit_code: i32) {
        self.event_log.add_event_tool_end(activity_kind, exit_code)
    }
    pub fn add_event_error(&mut self, activity_kind: ActivityKind, error: &NotionError) {
        self.event_log.add_event_error(activity_kind, error)
    }

    fn publish_to_event_log(mut self) {
        match publish_plugin(&self.hooks) {
            Ok(plugin) => {
                self.event_log.publish(plugin);
            }
            Err(e) => {
                eprintln!("Warning: invalid config file ({})", e);
            }
        }
    }

    pub fn exit(self, code: ExitCode) -> ! {
        self.publish_to_event_log();
        code.exit();
    }

    pub fn exit_tool(self, code: i32) -> ! {
        self.publish_to_event_log();
        exit(code);
    }
}

fn publish_plugin(hooks: &LazyHooks) -> Fallible<Option<&Publish>> {
    let hooks = hooks.get()?;
    Ok(hooks
        .events
        .as_ref()
        .and_then(|events| events.publish.as_ref()))
}

#[cfg(test)]
pub mod tests {

    use session::Session;
    use std::env;
    use std::path::PathBuf;

    fn fixture_path(fixture_dir: &str) -> PathBuf {
        let mut cargo_manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        cargo_manifest_dir.push("fixtures");
        cargo_manifest_dir.push(fixture_dir);
        cargo_manifest_dir
    }

    #[test]
    fn test_in_pinned_project() {
        let project_pinned = fixture_path("basic");
        env::set_current_dir(&project_pinned).expect("Could not set current directory");
        let pinned_session = Session::new().expect("Couldn't create new Session");
        assert_eq!(pinned_session.project_platform().is_some(), true);

        let project_unpinned = fixture_path("no_toolchain");
        env::set_current_dir(&project_unpinned).expect("Could not set current directory");
        let unpinned_session = Session::new().expect("Couldn't create new Session");
        assert_eq!(unpinned_session.project_platform().is_none(), true);
    }
}
