//! Provider registry plugin: mounts the built-in provider adapters.

use super::services::{PROVIDER_SERVICE, PROVIDER_SERVICE_ID, ProviderRegistry};
use crate::model::{
    ModelConfig, ModelFactory, ModelProtocol, ProviderCredentials, ProviderDescriptor,
    ProviderFieldDescriptor, ProviderFieldKind,
};
use crate::plugin::{
    DisposeError, Plugin, PluginContext, PluginDescriptor, PluginError, PluginId, ScopeKind,
    ServiceId,
};
use crate::providers::{OpenAiCompatibleModel, OpenAiModel};
use crate::{Model, ModelError};
use std::sync::Arc;

const REGISTRY_ID: PluginId = PluginId::new("builtin.provider_registry");
const RESPONSES_ID: PluginId = PluginId::new("builtin.openai_responses");
const COMPATIBLE_ID: PluginId = PluginId::new("builtin.openai_compatible");
const REGISTRY_PROVIDES: &[ServiceId] = &[PROVIDER_SERVICE_ID];
const REQUIRES_REGISTRY: &[ServiceId] = &[PROVIDER_SERVICE_ID];
const REGISTRY_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: REGISTRY_ID,
    scope: ScopeKind::TrustedProject,
    provides: REGISTRY_PROVIDES,
    requires: &[],
    optional: &[],
};
const RESPONSES_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: RESPONSES_ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES_REGISTRY,
    optional: &[],
};
const COMPATIBLE_DESCRIPTOR: PluginDescriptor = PluginDescriptor {
    id: COMPATIBLE_ID,
    scope: ScopeKind::TrustedProject,
    provides: &[],
    requires: REQUIRES_REGISTRY,
    optional: &[],
};

pub(crate) struct ProviderRegistryPlugin;

impl Plugin for ProviderRegistryPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &REGISTRY_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        context
            .provide(PROVIDER_SERVICE, Arc::new(ProviderRegistry::new()))
            .map_err(|error| PluginError::new(error.to_string()))
    }
}

struct ResponsesFactory;

impl ModelFactory for ResponsesFactory {
    fn protocol(&self) -> ModelProtocol {
        ModelProtocol::OpenAiResponses
    }

    fn describe(&self, credentials: &ProviderCredentials) -> ProviderDescriptor {
        descriptor(ModelProtocol::OpenAiResponses, credentials)
    }

    fn build(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        Ok(Box::new(OpenAiModel::from_runtime_fields(
            credentials.values().to_vec(),
            config.model.trim(),
            config.endpoint.trim(),
        )?))
    }
}

struct CompatibleFactory;

impl ModelFactory for CompatibleFactory {
    fn protocol(&self) -> ModelProtocol {
        ModelProtocol::OpenAiCompatible
    }

    fn describe(&self, credentials: &ProviderCredentials) -> ProviderDescriptor {
        descriptor(ModelProtocol::OpenAiCompatible, credentials)
    }

    fn build(
        &self,
        config: &ModelConfig,
        credentials: &ProviderCredentials,
    ) -> Result<Box<dyn Model>, ModelError> {
        Ok(Box::new(OpenAiCompatibleModel::from_runtime_fields(
            credentials.values().to_vec(),
            config,
        )?))
    }
}

fn descriptor(protocol: ModelProtocol, credentials: &ProviderCredentials) -> ProviderDescriptor {
    ProviderDescriptor {
        protocol,
        display_name: protocol.to_string(),
        fields: vec![ProviderFieldDescriptor {
            key: "api_key".into(),
            label: "API Key".into(),
            kind: ProviderFieldKind::Secret,
            required: false,
            sensitive: true,
            has_value: credentials.value(0).is_some_and(|value| !value.is_empty()),
        }],
    }
}

pub(crate) struct OpenAiResponsesPlugin;

impl Plugin for OpenAiResponsesPlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &RESPONSES_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        register_factory(context, Arc::new(ResponsesFactory))
    }
}

pub(crate) struct OpenAiCompatiblePlugin;

impl Plugin for OpenAiCompatiblePlugin {
    fn descriptor(&self) -> &'static PluginDescriptor {
        &COMPATIBLE_DESCRIPTOR
    }

    fn mount(&self, context: &mut PluginContext<'_>) -> Result<(), PluginError> {
        register_factory(context, Arc::new(CompatibleFactory))
    }
}

fn register_factory(
    context: &mut PluginContext<'_>,
    factory: Arc<dyn ModelFactory>,
) -> Result<(), PluginError> {
    let registry = context
        .require(PROVIDER_SERVICE)
        .map_err(|error| PluginError::new(error.to_string()))?;
    let lease = registry
        .register(context.owner(), factory)
        .map_err(|error| PluginError::new(error.to_string()))?;
    context.defer(move || {
        lease
            .revoke()
            .map_err(|error| DisposeError::new(error.to_string()))
    });
    Ok(())
}
