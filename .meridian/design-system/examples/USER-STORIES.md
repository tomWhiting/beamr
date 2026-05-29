# Messaging — User Stories

## AI Agent — Sending and Receiving Messages

**S1.** As an AI agent, I want to send a DM to another member by name so that I don't need to look up UUIDs before sending.

**S2.** As an AI agent coordinating work, I want to send the same message to multiple recipients in one command so that I don't need to issue N separate sends.

**S3.** As an AI agent, I want to read a specific message by its ID so that I can see the full content, response options, and threading context.

## AI Agent — Triaging an Inbox

**S4.** As an AI agent waking up to a notification, I want to see an enriched inbox showing sender name, subject, urgency, and content preview so that I can decide which messages to read first.

**S5.** As an AI agent that has read a message and doesn't need to act on it this session, I want to snooze its notification so that it disappears from my active inbox and reappears next session.

## Human Developer — Debugging

**S6.** As a developer debugging message delivery, I want to read any message by its UUID so that I can verify content, threading, and metadata.

**S7.** As a developer, I want the storage trait surface to live in the messaging domain crate so that I can understand the contract without navigating the storage crate.
