---
brachio: major
---

Follow the Rex status schema: use Idle for unrecognized states and foreground-only agents, remove Unknown status, and retain no-report source details.

Breaking change: Remove the public `brachio::model::Status::Unknown` variant.

Migration steps:

- Replace references to `Status::Unknown` with `Status::Idle`.
- Remove `Status::Unknown` match arms. Handle fallback states in the `Status::Idle` arm.
- Expect `Status::parse` to return `Status::Idle` for missing or unrecognized states.
