use crate::core::runtime_context::RuntimeContext;
use crate::core::variable_pool::{Selector, VariablePool};

use super::provider::MemoryError;
use super::types::MemoryScope;

pub fn resolve_namespace(
    scope: &MemoryScope,
    variable_pool: &VariablePool,
    context: &RuntimeContext,
    _node_id: &str,
) -> Result<String, MemoryError> {
    let namespace = match scope {
        MemoryScope::Conversation => {
            let conv_id = variable_pool
                .get(&Selector::new("sys", "conversation_id"))
                .as_string()
                .ok_or_else(|| MemoryError::ProviderError(
                    "sys.conversation_id not set; required for conversation scope".into(),
                ))?;
            format!("conv:{}", conv_id)
        }
        MemoryScope::User => {
            let user_id = variable_pool
                .get(&Selector::new("sys", "user_id"))
                .as_string()
                .ok_or_else(|| MemoryError::ProviderError(
                    "sys.user_id not set; required for user scope".into(),
                ))?;
            format!("user:{}", user_id)
        }
        MemoryScope::Global => {
            let app_id = variable_pool
                .get(&Selector::new("sys", "app_id"))
                .as_string()
                .unwrap_or_else(|| "default".to_string());
            format!("global:{}", app_id)
        }
    };

    #[cfg(feature = "security")]
    {
        if let Some(policy) = context
            .security_policy()
            .and_then(|p| p.memory.as_ref())
        {
            if !policy.allowed_namespace_prefixes.is_empty()
                && !policy
                    .allowed_namespace_prefixes
                    .iter()
                    .any(|prefix| namespace.starts_with(prefix))
            {
                return Err(MemoryError::AccessDenied { namespace });
            }
        }
    }

    Ok(namespace)
}
