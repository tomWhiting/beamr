---
name: meridian-identity
description: Retrieves the current member's identity and reporting details using the collective CLI. Use when you need to determine who you are in the workspace, get your session ID, find your reporting chain, or look up member information.
---

## Purpose

Use this skill to identify yourself within the collective workspace and retrieve your reporting chain and member details.

## How to determine your identity

Run the following command using your current session ID:

```bash
collective member info ${CLAUDE_SESSION_ID} --text
```


## What the output includes

<member name>
  [<member kind>] - Session ID: <session-uuid>

Member Details:
  ID:             <member id> // for AI members: The member and session id should be the same.
  Name:           <your full member name as registered in meridian> // The full name must be used when messaging another member.
  Workdir:        <your root directory>
  Workspace:      <the id of the workspace(s) you belongs to>
  Joined:         <ISO 8601 timestamp when joined>

Reporting:
  Manager:        <name of your manager, or "-" if none>
  Direct Reports:
                - <name and uuid of each member who reports you>

Current Status:   <activity status, e.g. active, available or busy>
  Focus:          <focus of your work> // this shows focus as last set, however it may be stale and need updating.
  Last Seen:      <ISO 8601 timestamp of last activity>
