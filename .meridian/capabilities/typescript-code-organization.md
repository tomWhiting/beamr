---
name: typescript-code-organization
description: TypeScript/React code organization standards — feature folders, barrel exports, no god files, component composition. Adds guidelines for how TypeScript and React code should be organized.
tools: Read, Glob, Grep
---

# TypeScript / React Code Organization

## Directory Structure

```
src/
  components/
    feature-name/
      index.ts              # barrel export only
      FeatureComponent.tsx   # main component
      FeaturePanel.tsx       # sub-components
      useFeatureHook.ts      # hooks
      types.ts               # types and interfaces
      helpers.ts             # pure utility functions
      constants.ts           # feature-specific constants
    ui/                      # shared UI primitives
      Button.tsx
      Dialog.tsx
      index.ts
  hooks/                     # shared hooks (not feature-specific)
    useWebSocket.ts
    useInfiniteQuery.ts
    index.ts
  stores/                    # state management
    feature-store.ts
    index.ts
  lib/                       # shared utilities
    api.ts
    formatting.ts
    index.ts
  types/                     # shared type definitions
    api.ts
    models.ts
    index.ts
```

## Rules

### Feature folders, not file type folders

Group by feature, not by file type. A developer working on "auth" should find everything in one place.

**Bad:**
```
src/
  components/
    AuthForm.tsx
    AuthButton.tsx
    UserProfile.tsx
    UserAvatar.tsx
  hooks/
    useAuth.ts
    useUser.ts
  types/
    auth.ts
    user.ts
```

**Good:**
```
src/
  components/
    auth/
      index.ts
      AuthForm.tsx
      AuthButton.tsx
      useAuth.ts
      types.ts
    user/
      index.ts
      UserProfile.tsx
      UserAvatar.tsx
      useUser.ts
      types.ts
```

### index.ts is for barrel exports, not code

An `index.ts` file should contain ONLY re-exports:

```typescript
export { AuthForm } from './AuthForm';
export { AuthButton } from './AuthButton';
export { useAuth } from './useAuth';
export type { AuthState, AuthUser } from './types';
```

It should NOT contain component definitions, hooks, utilities, or any logic. If you're writing code in `index.ts`, move it to a named file.

### No god files

No single file should exceed ~300 lines. If a component file is longer:

- Extract sub-components into their own files
- Extract hooks into `useFeatureName.ts`
- Extract types into `types.ts`
- Extract helper functions into `helpers.ts`

Signs a file needs splitting:
- Multiple component definitions in one file
- A component with more than 2-3 hooks of its own logic
- Types/interfaces that could be shared
- Utility functions mixed with component code
- You need to scroll to find the component's return statement

### Component composition over monoliths

A component that handles layout, data fetching, state management, AND rendering is doing too much. Split into:

- **Container**: data fetching, state, side effects
- **Presentation**: pure rendering from props
- **Hook**: encapsulated stateful logic

```typescript
// useChannelMessages.ts — hook handles the data
export function useChannelMessages(channelId: string) { ... }

// ChannelMessages.tsx — component handles the rendering
export function ChannelMessages({ channelId }: Props) {
  const { messages, isLoading } = useChannelMessages(channelId);
  return <MessageList messages={messages} loading={isLoading} />;
}
```

### Types live close to their usage

- Types used by ONE feature → `feature/types.ts`
- Types shared across features → `src/types/`
- API response types → `src/types/api.ts`
- Types generated from Rust (via ts-rs) → keep in their generated location, re-export from `src/types/`

Do not put all types in a single `types.ts` at the root. That creates a god file that everything imports from.

### Hooks follow naming conventions

- Feature-specific hooks: `feature/useFeatureName.ts`
- Shared hooks: `src/hooks/useHookName.ts`
- Always prefix with `use`
- One hook per file (unless they're trivially small and tightly coupled)

### State management

- Zustand stores: one file per store in `src/stores/`
- Store files should export the hook AND the type: `export const useAuthStore = create<AuthState>(...)`
- Do not put component logic in stores — stores hold state, components hold behavior

### Imports

- Use path aliases (`@/components/...`) over relative paths when crossing feature boundaries
- Relative imports within a feature folder are fine
- Never import from another feature's internal files — only through its barrel export

**Bad:**
```typescript
import { formatDate } from '../../user/helpers';
```

**Good:**
```typescript
import { formatDate } from '@/lib/formatting';
// or if it's feature-specific, it shouldn't be imported cross-feature
```

### When to create a new feature folder vs add to existing

Create a new feature folder when:
- The code handles a distinct UI concern (sidebar, command palette, editor)
- It has its own state, hooks, and types
- It could be understood without reading sibling features

Add to an existing feature folder when:
- The code is a sub-component or variation of an existing feature
- It shares state and types with the existing feature
- Splitting it would create artificial boundaries
