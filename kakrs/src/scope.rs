//! Port of scope.hh/cc

use crate::alias_registry::AliasRegistry;
use crate::context::Context;
use crate::exception::Result;
use crate::face::FaceRegistry;
use crate::highlighter_group::Highlighters;
use crate::hook_manager::{Hook, HookManager};
use crate::keymap_manager::KeymapManager;
use crate::option_manager::{Opt, OptionManager, OptionWatcher, OptionsRegistry};
use std::cell::OnceCell;
use std::rc::Rc;

#[derive(Clone)]
pub struct Scope {
    options: Rc<OptionManager>,
    hooks: Rc<HookManager>,
    keymaps: Rc<KeymapManager>,
    aliases: Rc<AliasRegistry>,
    faces: Rc<FaceRegistry>,
    highlighters: Rc<Highlighters>,
}

impl Scope {
    pub fn new_root() -> Scope {
        Scope {
            options: OptionManager::new_root(),
            hooks: Rc::new(HookManager::new_root()),
            keymaps: Rc::new(KeymapManager::default()),
            aliases: Rc::new(AliasRegistry::default()),
            faces: Rc::new(FaceRegistry::new_root()),
            highlighters: Rc::new(Highlighters::new_root()),
        }
    }

    pub fn new(parent: &Scope) -> Scope {
        Scope {
            options: OptionManager::new(&parent.options),
            hooks: Rc::new(HookManager::new(&parent.hooks)),
            keymaps: Rc::new(KeymapManager::new(&parent.keymaps)),
            aliases: Rc::new(AliasRegistry::new(&parent.aliases)),
            faces: Rc::new(FaceRegistry::new(&parent.faces)),
            highlighters: Rc::new(Highlighters::new(&parent.highlighters)),
        }
    }

    pub fn options(&self) -> &Rc<OptionManager> {
        &self.options
    }
    pub fn hooks(&self) -> &Rc<HookManager> {
        &self.hooks
    }
    pub fn keymaps(&self) -> &Rc<KeymapManager> {
        &self.keymaps
    }
    pub fn aliases(&self) -> &Rc<AliasRegistry> {
        &self.aliases
    }
    pub fn faces(&self) -> &Rc<FaceRegistry> {
        &self.faces
    }
    pub fn highlighters(&self) -> &Rc<Highlighters> {
        &self.highlighters
    }

    pub fn reparent(&self, parent: &Scope) {
        self.options.reparent(&parent.options);
        self.hooks.reparent(&parent.hooks);
        self.keymaps.reparent(&parent.keymaps);
        self.aliases.reparent(&parent.aliases);
        self.faces.reparent(&parent.faces);
        self.highlighters.reparent(&parent.highlighters);
    }
}

pub struct GlobalScope {
    scope: Scope,
    option_registry: OptionsRegistry,
    watcher: Rc<GlobalOptionWatcher>,
}

struct GlobalOptionWatcher;

impl OptionWatcher for GlobalOptionWatcher {
    fn on_option_changed(&self, option: &Opt) -> Result<()> {
        let empty_context = Context::new_empty();
        GlobalScope::instance().scope.hooks().run_hook(
            Hook::GlobalSetOption,
            &kformat!("{}={}", option.name(), option.get_desc_string()),
            &empty_context,
        )
    }
}

thread_local! {
    static GLOBAL_SCOPE: OnceCell<&'static GlobalScope> = const { OnceCell::new() };
}

impl GlobalScope {
    pub fn create() -> &'static GlobalScope {
        let scope = Scope::new_root();
        let option_registry = OptionsRegistry::new(scope.options().clone());
        let watcher = Rc::new(GlobalOptionWatcher);
        let gs: &'static GlobalScope = Box::leak(Box::new(GlobalScope { scope, option_registry, watcher }));
        gs.scope
            .options()
            .register_watcher(Rc::downgrade(&gs.watcher) as std::rc::Weak<dyn OptionWatcher>);
        GLOBAL_SCOPE.with(|g| {
            let _ = g.set(gs);
        });
        gs
    }

    pub fn instance() -> &'static GlobalScope {
        GLOBAL_SCOPE.with(|g| *g.get().expect("GlobalScope not created"))
    }

    pub fn has_instance() -> bool {
        GLOBAL_SCOPE.with(|g| g.get().is_some())
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    pub fn option_registry(&self) -> &OptionsRegistry {
        &self.option_registry
    }

    pub fn options(&self) -> &Rc<OptionManager> {
        self.scope.options()
    }
    pub fn hooks(&self) -> &Rc<HookManager> {
        self.scope.hooks()
    }
    pub fn keymaps(&self) -> &Rc<KeymapManager> {
        self.scope.keymaps()
    }
    pub fn aliases(&self) -> &Rc<AliasRegistry> {
        self.scope.aliases()
    }
    pub fn faces(&self) -> &Rc<FaceRegistry> {
        self.scope.faces()
    }
    pub fn highlighters(&self) -> &Rc<Highlighters> {
        self.scope.highlighters()
    }
}
