# Messaging — Checklist

## Crate Setup

- [x] **C1** — libmessage crate exists as a workspace member
- [x] **C2** — Cargo.toml declares the crate as libmessage with edition 2021
- [x] **C3** — src/lib.rs declares public modules: types, protocol, events, storage, ops, invariants, validation, mentions, threading

## Storage Extraction

- [ ] **C4** — StorageError defined in libmessage::storage::error
- [ ] **C5** — MessagingStorage trait defined in libmessage::storage::traits
- [ ] **C6** — All model types moved to libmessage::storage::models
- [ ] **C7** — Notification bug fix: read-status update clears notification entry

## Domain Operations

- [ ] **C8** — Domain operations moved to libmessage::ops
- [ ] **C9** — Thin service wrapper delegates to libmessage ops
