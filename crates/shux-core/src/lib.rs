//! shux core — daemon, data model, event bus, config, theme engine.

pub mod bus;
pub mod config;
pub mod daemon;
pub mod event;
pub mod graph;
pub mod layout;
pub mod model;
pub mod theme;

// Re-export key event bus types.
pub use bus::{EventBus, EventBusConfig, Subscription, SubscriptionEvent};
pub use event::{ClientId, ConfigChange, Event, EventData, EventMetadata};
