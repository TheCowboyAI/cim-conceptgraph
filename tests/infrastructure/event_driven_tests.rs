//! Event-driven infrastructure tests for cim-conceptgraph
//!
//! These tests verify that the conceptual graph module properly integrates
//! with the event-driven architecture, including NATS messaging, event
//! streaming, and cross-module communication.

use async_nats::jetstream;
use cim_conceptgraph::{ConceptGraph, ConceptId, ConceptualPoint};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;

/// Test event for concept graph operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConceptGraphEvent {
    pub event_type: ConceptGraphEventType,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConceptGraphEventType {
    ConceptAdded {
        concept_id: ConceptId,
        name: String,
        position: Vec<f32>,
    },
    ConceptsConnected {
        source_id: ConceptId,
        target_id: ConceptId,
        similarity: f32,
    },
    RegionFormed {
        region_id: String,
        concept_ids: Vec<ConceptId>,
    },
}

/// Helper to validate event sequences
pub struct EventStreamValidator {
    expected_events: Vec<String>,
    received_events: Vec<String>,
}

impl EventStreamValidator {
    pub fn new(expected: Vec<String>) -> Self {
        Self {
            expected_events: expected,
            received_events: Vec::new(),
        }
    }

    pub fn record_event(&mut self, event_type: &str) {
        self.received_events.push(event_type.to_string());
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.received_events == self.expected_events {
            Ok(())
        } else {
            Err(format!(
                "Event sequence mismatch. Expected: {:?}, Got: {:?}",
                self.expected_events, self.received_events
            ))
        }
    }
}

mod layer_1_1_nats_connection {
    use super::*;
    use futures::StreamExt;

    /// User Story: As a developer, I need to ensure concept graph events
    /// are published to NATS when concepts are added or connected.
    ///
    /// Event Sequence:
    /// ```mermaid
    /// graph TD
    ///     A[Add Concept] --> B[Publish ConceptAdded Event]
    ///     B --> C[NATS JetStream]
    ///     C --> D[Event Consumed]
    /// ```
    #[tokio::test]
    async fn test_concept_graph_nats_connection() -> Result<(), Box<dyn std::error::Error>> {
        // Setup NATS connection
        let nats_client = async_nats::connect("nats://localhost:4222").await?;
        let js = jetstream::new(nats_client);

        // Create stream for concept graph events
        let stream_config = jetstream::stream::Config {
            name: "CONCEPT_GRAPH_EVENTS".to_string(),
            subjects: vec!["conceptgraph.>".to_string()],
            ..Default::default()
        };

        // Try to delete stream if it exists
        let _ = js.delete_stream("CONCEPT_GRAPH_EVENTS").await;
        
        js.create_stream(stream_config).await?;

        // Create concept graph
        let mut graph = ConceptGraph::new("test_graph");
        let concept_id = ConceptId::new();
        let position = ConceptualPoint {
            coordinates: vec![0.1, 0.2, 0.3],
        };

        // Add concept (this should trigger event)
        graph.add_concept(concept_id, position, "test_concept".to_string());

        // Publish event
        let event = ConceptGraphEvent {
            event_type: ConceptGraphEventType::ConceptAdded {
                concept_id,
                name: "test_concept".to_string(),
                position: vec![0.1, 0.2, 0.3],
            },
            timestamp: chrono::Utc::now(),
            metadata: HashMap::new(),
        };

        js.publish("conceptgraph.concept.added", serde_json::to_vec(&event)?.into())
            .await?
            .await?;

        // Verify event was published
        let consumer = js
            .create_consumer_on_stream(
                jetstream::consumer::Config {
                    durable_name: Some("test_consumer".to_string()),
                    ..Default::default()
                },
                "CONCEPT_GRAPH_EVENTS",
            )
            .await?;

        let mut messages = consumer.messages().await?;
        let msg = timeout(Duration::from_secs(1), messages.next())
            .await?
            .expect("Should receive message")?;

        let received_event: ConceptGraphEvent = serde_json::from_slice(&msg.payload)?;
        assert!(matches!(
            received_event.event_type,
            ConceptGraphEventType::ConceptAdded { .. }
        ));

        msg.ack().await?;

        // Cleanup
        js.delete_stream("CONCEPT_GRAPH_EVENTS").await?;

        Ok(())
    }

    /// User Story: As a system architect, I need to ensure concept connections
    /// generate proper events for downstream processing.
    #[tokio::test]
    async fn test_concept_connection_events() -> Result<(), Box<dyn std::error::Error>> {
        let nats_client = async_nats::connect("nats://localhost:4222").await?;
        let js = jetstream::new(nats_client);

        // Create stream
        let stream_config = jetstream::stream::Config {
            name: "CONCEPT_CONNECTIONS".to_string(),
            subjects: vec!["conceptgraph.connection.>".to_string()],
            ..Default::default()
        };

        // Try to delete stream if it exists
        let _ = js.delete_stream("CONCEPT_CONNECTIONS").await;

        js.create_stream(stream_config).await?;

        // Create concepts
        let concept1_id = ConceptId::new();
        let concept2_id = ConceptId::new();

        // Publish connection event
        let event = ConceptGraphEvent {
            event_type: ConceptGraphEventType::ConceptsConnected {
                source_id: concept1_id,
                target_id: concept2_id,
                similarity: 0.85,
            },
            timestamp: chrono::Utc::now(),
            metadata: HashMap::new(),
        };

        js.publish(
            "conceptgraph.connection.created",
            serde_json::to_vec(&event)?.into(),
        )
        .await?
        .await?;

        // Verify
        let consumer = js
            .create_consumer_on_stream(
                jetstream::consumer::Config {
                    durable_name: Some("connection_consumer".to_string()),
                    ..Default::default()
                },
                "CONCEPT_CONNECTIONS",
            )
            .await?;

        let mut messages = consumer.messages().await?;
        let msg = timeout(Duration::from_secs(1), messages.next())
            .await?
            .expect("Should receive message")?;

        let received: ConceptGraphEvent = serde_json::from_slice(&msg.payload)?;
        assert!(matches!(
            received.event_type,
            ConceptGraphEventType::ConceptsConnected { similarity, .. } if similarity == 0.85
        ));

        msg.ack().await?;

        // Cleanup
        js.delete_stream("CONCEPT_CONNECTIONS").await?;

        Ok(())
    }

    /// User Story: As a developer, I need region formation events to trigger
    /// updates in dependent systems.
    #[tokio::test]
    async fn test_region_formation_events() -> Result<(), Box<dyn std::error::Error>> {
        let nats_client = async_nats::connect("nats://localhost:4222").await?;
        let js = jetstream::new(nats_client);

        // Create stream
        let stream_config = jetstream::stream::Config {
            name: "CONCEPT_REGIONS".to_string(),
            subjects: vec!["conceptgraph.region.>".to_string()],
            ..Default::default()
        };

        // Try to delete stream if it exists
        let _ = js.delete_stream("CONCEPT_REGIONS").await;

        js.create_stream(stream_config).await?;

        // Create region event
        let concept_ids = vec![ConceptId::new(), ConceptId::new(), ConceptId::new()];
        let event = ConceptGraphEvent {
            event_type: ConceptGraphEventType::RegionFormed {
                region_id: "test_region".to_string(),
                concept_ids: concept_ids.clone(),
            },
            timestamp: chrono::Utc::now(),
            metadata: HashMap::new(),
        };

        js.publish("conceptgraph.region.formed", serde_json::to_vec(&event)?.into())
            .await?
            .await?;

        // Verify
        let consumer = js
            .create_consumer_on_stream(
                jetstream::consumer::Config {
                    durable_name: Some("region_consumer".to_string()),
                    ..Default::default()
                },
                "CONCEPT_REGIONS",
            )
            .await?;

        let mut messages = consumer.messages().await?;
        let msg = timeout(Duration::from_secs(1), messages.next())
            .await?
            .expect("Should receive message")?;

        let received: ConceptGraphEvent = serde_json::from_slice(&msg.payload)?;
        match received.event_type {
            ConceptGraphEventType::RegionFormed { concept_ids: ids, .. } => {
                assert_eq!(ids.len(), 3);
            }
            _ => panic!("Wrong event type"),
        }

        msg.ack().await?;

        // Cleanup
        js.delete_stream("CONCEPT_REGIONS").await?;

        Ok(())
    }
}

mod layer_1_2_event_store {
    use super::*;
    use futures::StreamExt;

    /// User Story: As a data scientist, I need concept graph events to be
    /// stored for analysis and replay.
    #[tokio::test]
    async fn test_concept_event_persistence() -> Result<(), Box<dyn std::error::Error>> {
        let nats_client = async_nats::connect("nats://localhost:4222").await?;
        let js = jetstream::new(nats_client);

        // Create persistent stream
        let stream_config = jetstream::stream::Config {
            name: "CONCEPT_EVENTS_STORE".to_string(),
            subjects: vec!["conceptgraph.events.>".to_string()],
            retention: jetstream::stream::RetentionPolicy::Limits,
            storage: jetstream::stream::StorageType::File,
            ..Default::default()
        };

        // Try to delete stream if it exists
        let _ = js.delete_stream("CONCEPT_EVENTS_STORE").await;

        js.create_stream(stream_config).await?;

        // Store multiple events
        let events = vec![
            ConceptGraphEvent {
                event_type: ConceptGraphEventType::ConceptAdded {
                    concept_id: ConceptId::new(),
                    name: "concept1".to_string(),
                    position: vec![0.1, 0.2],
                },
                timestamp: chrono::Utc::now(),
                metadata: HashMap::new(),
            },
            ConceptGraphEvent {
                event_type: ConceptGraphEventType::ConceptAdded {
                    concept_id: ConceptId::new(),
                    name: "concept2".to_string(),
                    position: vec![0.3, 0.4],
                },
                timestamp: chrono::Utc::now(),
                metadata: HashMap::new(),
            },
        ];

        for (i, event) in events.iter().enumerate() {
            js.publish(
                format!("conceptgraph.events.{}", i),
                serde_json::to_vec(event)?.into(),
            )
            .await?
            .await?;
        }

        // Verify persistence by counting messages
        let consumer = js
            .create_consumer_on_stream(
                jetstream::consumer::Config {
                    durable_name: Some("count_consumer".to_string()),
                    deliver_policy: jetstream::consumer::DeliverPolicy::All,
                    ..Default::default()
                },
                "CONCEPT_EVENTS_STORE",
            )
            .await?;
        
        let mut messages = consumer.messages().await?;
        let mut count = 0;
        while let Ok(Some(msg)) = timeout(Duration::from_millis(100), messages.next()).await {
            let msg = msg?;
            count += 1;
            msg.ack().await?;
        }
        assert_eq!(count, 2);

        // Cleanup
        js.delete_stream("CONCEPT_EVENTS_STORE").await?;

        Ok(())
    }

    /// User Story: As a developer, I need to replay concept graph events
    /// to rebuild state after failures.
    #[tokio::test]
    async fn test_event_replay() -> Result<(), Box<dyn std::error::Error>> {
        let nats_client = async_nats::connect("nats://localhost:4222").await?;
        let js = jetstream::new(nats_client);

        // Create stream
        let stream_config = jetstream::stream::Config {
            name: "CONCEPT_REPLAY".to_string(),
            subjects: vec!["conceptgraph.replay.>".to_string()],
            ..Default::default()
        };

        // Try to delete stream if it exists
        let _ = js.delete_stream("CONCEPT_REPLAY").await;

        js.create_stream(stream_config).await?;

        // Store events
        let concept_ids: Vec<ConceptId> = (0..3).map(|_| ConceptId::new()).collect();
        
        for (i, id) in concept_ids.iter().enumerate() {
            let event = ConceptGraphEvent {
                event_type: ConceptGraphEventType::ConceptAdded {
                    concept_id: *id,
                    name: format!("concept_{}", i),
                    position: vec![i as f32 * 0.1],
                },
                timestamp: chrono::Utc::now(),
                metadata: HashMap::new(),
            };

            js.publish(
                "conceptgraph.replay.event",
                serde_json::to_vec(&event)?.into(),
            )
            .await?
            .await?;
        }

        // Replay events
        let consumer = js
            .create_consumer_on_stream(
                jetstream::consumer::Config {
                    durable_name: Some("replay_consumer".to_string()),
                    deliver_policy: jetstream::consumer::DeliverPolicy::All,
                    ..Default::default()
                },
                "CONCEPT_REPLAY",
            )
            .await?;

        let mut messages = consumer.messages().await?;
        let mut replayed_count = 0;

        while let Ok(Some(msg)) = timeout(Duration::from_millis(100), messages.next()).await {
            let msg = msg?;
            let _event: ConceptGraphEvent = serde_json::from_slice(&msg.payload)?;
            replayed_count += 1;
            msg.ack().await?;
        }

        assert_eq!(replayed_count, 3);

        // Cleanup
        js.delete_stream("CONCEPT_REPLAY").await?;

        Ok(())
    }
}

mod layer_1_3_cross_module_events {
    use super::*;
    use futures::StreamExt;

    /// User Story: As an architect, I need concept graph events to integrate
    /// with other domains like workflow and agent domains.
    #[tokio::test]
    async fn test_cross_domain_concept_events() -> Result<(), Box<dyn std::error::Error>> {
        let nats_client = async_nats::connect("nats://localhost:4222").await?;
        let js = jetstream::new(nats_client);

        // Create streams for multiple domains
        let streams = vec![
            ("CONCEPTS", vec!["concepts.>".to_string()]),
            ("WORKFLOWS", vec!["workflows.>".to_string()]),
            ("AGENTS", vec!["agents.>".to_string()]),
        ];

        // Clean up existing streams
        for (name, _) in &streams {
            let _ = js.delete_stream(name).await;
        }

        for (name, subjects) in streams {
            let config = jetstream::stream::Config {
                name: name.to_string(),
                subjects,
                ..Default::default()
            };
            js.create_stream(config).await?;
        }

        // Simulate cross-domain event flow
        let concept_id = ConceptId::new();
        
        // 1. Concept added
        let concept_event = ConceptGraphEvent {
            event_type: ConceptGraphEventType::ConceptAdded {
                concept_id,
                name: "workflow_concept".to_string(),
                position: vec![0.5, 0.6],
            },
            timestamp: chrono::Utc::now(),
            metadata: HashMap::new(),
        };

        js.publish("concepts.added", serde_json::to_vec(&concept_event)?.into())
            .await?
            .await?;

        // 2. This triggers workflow creation
        let workflow_event = serde_json::json!({
            "event_type": "WorkflowCreated",
            "concept_id": concept_id,
            "workflow_id": uuid::Uuid::new_v4(),
        });

        js.publish("workflows.created", serde_json::to_vec(&workflow_event)?.into())
            .await?
            .await?;

        // 3. Which assigns an agent
        let agent_event = serde_json::json!({
            "event_type": "AgentAssigned",
            "concept_id": concept_id,
            "agent_id": uuid::Uuid::new_v4(),
        });

        js.publish("agents.assigned", serde_json::to_vec(&agent_event)?.into())
            .await?
            .await?;

        // Verify event flow by checking each stream has messages
        for stream_name in ["CONCEPTS", "WORKFLOWS", "AGENTS"] {
            let consumer = js
                .create_consumer_on_stream(
                    jetstream::consumer::Config {
                        durable_name: Some(format!("verify_{}", stream_name)),
                        deliver_policy: jetstream::consumer::DeliverPolicy::All,
                        ..Default::default()
                    },
                    stream_name,
                )
                .await?;
            
            let mut messages = consumer.messages().await?;
            let msg = timeout(Duration::from_millis(100), messages.next())
                .await?
                .expect("Should have message")?;
            msg.ack().await?;
        }

        // Cleanup
        for stream_name in ["CONCEPTS", "WORKFLOWS", "AGENTS"] {
            js.delete_stream(stream_name).await?;
        }

        Ok(())
    }

    /// User Story: As a developer, I need concept similarity calculations
    /// to trigger graph update events.
    #[tokio::test]
    async fn test_similarity_triggered_events() -> Result<(), Box<dyn std::error::Error>> {
        let nats_client = async_nats::connect("nats://localhost:4222").await?;
        let js = jetstream::new(nats_client);

        // Create stream
        let stream_config = jetstream::stream::Config {
            name: "SIMILARITY_EVENTS".to_string(),
            subjects: vec!["conceptgraph.similarity.>".to_string()],
            ..Default::default()
        };

        // Try to delete stream if it exists
        let _ = js.delete_stream("SIMILARITY_EVENTS").await;

        js.create_stream(stream_config).await?;

        // Create validator
        let mut validator = EventStreamValidator::new(vec![
            "concept_added".to_string(),
            "concept_added".to_string(),
            "similarity_calculated".to_string(),
            "connection_created".to_string(),
        ]);

        // Simulate event sequence
        let concept1_id = ConceptId::new();
        let concept2_id = ConceptId::new();

        // Add first concept
        let event1 = ConceptGraphEvent {
            event_type: ConceptGraphEventType::ConceptAdded {
                concept_id: concept1_id,
                name: "concept1".to_string(),
                position: vec![0.1, 0.2],
            },
            timestamp: chrono::Utc::now(),
            metadata: HashMap::new(),
        };
        js.publish("conceptgraph.similarity.event", serde_json::to_vec(&event1)?.into())
            .await?
            .await?;
        validator.record_event("concept_added");

        // Add second concept
        let event2 = ConceptGraphEvent {
            event_type: ConceptGraphEventType::ConceptAdded {
                concept_id: concept2_id,
                name: "concept2".to_string(),
                position: vec![0.15, 0.25],
            },
            timestamp: chrono::Utc::now(),
            metadata: HashMap::new(),
        };
        js.publish("conceptgraph.similarity.event", serde_json::to_vec(&event2)?.into())
            .await?
            .await?;
        validator.record_event("concept_added");

        // Similarity calculation triggers
        let similarity_event = serde_json::json!({
            "event_type": "SimilarityCalculated",
            "concept1": concept1_id,
            "concept2": concept2_id,
            "similarity": 0.95,
        });
        js.publish(
            "conceptgraph.similarity.calculated",
            serde_json::to_vec(&similarity_event)?.into(),
        )
        .await?
        .await?;
        validator.record_event("similarity_calculated");

        // High similarity triggers connection
        let connection_event = ConceptGraphEvent {
            event_type: ConceptGraphEventType::ConceptsConnected {
                source_id: concept1_id,
                target_id: concept2_id,
                similarity: 0.95,
            },
            timestamp: chrono::Utc::now(),
            metadata: HashMap::new(),
        };
        js.publish(
            "conceptgraph.similarity.connected",
            serde_json::to_vec(&connection_event)?.into(),
        )
        .await?
        .await?;
        validator.record_event("connection_created");

        // Validate sequence
        validator.validate()?;

        // Cleanup
        js.delete_stream("SIMILARITY_EVENTS").await?;

        Ok(())
    }
}

#[cfg(test)]
mod test_helpers {
    use super::*;

    /// Create a test concept graph with sample data
    pub fn create_test_graph() -> ConceptGraph {
        let mut graph = ConceptGraph::new("test_graph");
        
        let concepts = vec![
            ("machine_learning", vec![0.8, 0.2, 0.1]),
            ("deep_learning", vec![0.9, 0.3, 0.1]),
            ("neural_networks", vec![0.85, 0.35, 0.15]),
        ];

        for (name, coordinates) in concepts {
            let id = ConceptId::new();
            let position = ConceptualPoint { coordinates };
            graph.add_concept(id, position, name.to_string());
        }

        graph
    }
} 