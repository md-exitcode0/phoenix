//! Provider registry and factory

use super::providers_data::{self, ProviderModels};

pub struct ProviderRegistry;

impl ProviderRegistry {
    pub fn new() -> Self {
        Self
    }

    pub fn list(&self) -> Vec<ProviderModels> {
        providers_data::all_providers()
    }

    pub fn get(&self, name: &str) -> Option<ProviderModels> {
        providers_data::get_provider(name)
    }

    pub fn list_all_providers() -> Vec<ProviderModels> {
        providers_data::all_providers()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}
