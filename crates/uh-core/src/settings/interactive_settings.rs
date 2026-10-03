//! The settings-screen accessors (TS settings-manager getters and setters
//! behind the `/settings` rows). Every setter writes the global scope like
//! the TS `markModified` + `save` pair; every getter reads the merged
//! effective settings with the TS defaults.

use anyhow::Result;

use super::manager::SettingsManager;

impl SettingsManager {
    /// `images.autoResize` (TS default true).
    #[must_use]
    pub fn get_image_auto_resize(&self) -> bool {
        self.settings()
            .images
            .as_ref()
            .and_then(|images| images.auto_resize)
            .unwrap_or(true)
    }

    /// `images.autoResize` setter.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings scope cannot be saved.
    pub fn set_image_auto_resize(&mut self, enabled: bool) -> Result<()> {
        self.global_mut()
            .images
            .get_or_insert_with(Default::default)
            .auto_resize = Some(enabled);
        self.save_global_scope()
    }

    /// `images.blockImages` (TS default false).
    #[must_use]
    pub fn get_block_images(&self) -> bool {
        self.settings()
            .images
            .as_ref()
            .and_then(|images| images.block_images)
            .unwrap_or(false)
    }

    /// `images.blockImages` setter.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings scope cannot be saved.
    pub fn set_block_images(&mut self, blocked: bool) -> Result<()> {
        self.global_mut()
            .images
            .get_or_insert_with(Default::default)
            .block_images = Some(blocked);
        self.save_global_scope()
    }

    /// `enableSkillCommands` (TS default true).
    #[must_use]
    pub fn get_enable_skill_commands(&self) -> bool {
        self.settings().enable_skill_commands.unwrap_or(true)
    }

    /// `enableSkillCommands` setter.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings scope cannot be saved.
    pub fn set_enable_skill_commands(&mut self, enabled: bool) -> Result<()> {
        self.global_mut().enable_skill_commands = Some(enabled);
        self.save_global_scope()
    }

    /// `enableBuiltinSkills` (TS default true).
    #[must_use]
    pub fn get_enable_builtin_skills(&self) -> bool {
        self.settings().enable_builtin_skills.unwrap_or(true)
    }

    /// `enableBuiltinSkills` setter.
    ///
    /// # Errors
    ///
    /// Returns an error when the global settings scope cannot be saved.
    pub fn set_enable_builtin_skills(&mut self, enabled: bool) -> Result<()> {
        self.global_mut().enable_builtin_skills = Some(enabled);
        self.save_global_scope()
    }
}
