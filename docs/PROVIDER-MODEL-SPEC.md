# Provider Model Specification

## Purpose

This document defines xi-agent's backend model.

It specifies:

- the domain concepts xi-agent uses for backend integrations
- the relationship between provider, service, API, endpoint, preset, and instance
- how the interactive UI presents those concepts
- how configuration represents those concepts
- how built-in and custom backends fit the same model

This document is authoritative for terminology and behavior.

## Domain model

### Provider

A **provider** is the organization or operator that runs and exposes a backend.

A provider is responsible for one or more of:

- operating the service
- issuing credentials
- billing users
- deciding which models are available

Examples:

- OpenRouter
- OpenAI
- GitHub
- Google
- an organization operating an internal Open WebUI deployment

### Service

A **service** is the backend product or software xi-agent talks to.

A service defines:

- backend behavior
- model-routing rules
- supported APIs
- authentication expectations
- endpoint structure

Examples:

- OpenRouter
- OpenAI API
- GitHub Copilot
- Gemini / Cloud Code Assist
- Ollama
- Open WebUI

A service may be:

- a hosted product with predetermined endpoints
- deployable software running at a user-supplied endpoint

### API

An **API** is the protocol surface xi-agent uses to communicate with a service.

Examples:

- OpenAI-compatible
- OpenAI Responses
- Anthropic-compatible
- Gemini native
- Ollama chat API

The API determines:

- request shape
- response shape
- streaming format
- tool-calling behavior
- model selection semantics exposed to xi-agent

### Endpoint

An **endpoint** is the concrete URL where xi-agent reaches an API.

Examples:

- `https://openrouter.ai/api/v1`
- `https://api.openai.com/v1`
- `http://localhost:11434`
- `https://my-webui.example.com/api`

An endpoint is either:

- **predetermined** by the provider/service integration
- **user-supplied** during provider instance setup

### Provider preset

A **provider preset** is xi-agent's built-in definition of a recognized backend kind.

A provider preset defines:

- label
- provider identity
- service identity
- supported APIs
- default API
- whether API selection is user-visible
- whether the endpoint is predetermined or user-supplied
- whether multiple configured instances are valid
- authentication mode
- default model
- backend-specific behavior and constraints

Examples of provider presets:

- OpenRouter
- OpenAI
- Copilot
- Codex
- Gemini
- Ollama
- Open WebUI
- generic OpenAI-compatible endpoint

### Provider instance

A **provider instance** is one configured, selectable backend entry in xi-agent.

A provider instance contains:

- a stable id
- a provider preset
- a selected API
- an endpoint, if required
- credentials, if required
- a selected/default model

A provider instance is the unit that appears in the main provider picker.

Examples:

- `copilot`
- `openrouter`
- `codex`
- `gemini`
- `gpu-box`
- `work-webui`
- `lab-router`

A provider instance is either:

- a **built-in instance**, always available from the static backend catalog
- a **custom instance**, created by the user through the login flow

## Backend classes

Xi-agent supports two backend classes.

### Built-in hosted providers

A built-in hosted provider has a recognized provider identity, a recognized
service identity, and a predetermined endpoint family.

Properties:

- always available in the provider picker from the static catalog, even on
  a clean install with no config file
- does not require the user to invent a custom instance just to use the normal
  hosted service
- may still require credentials before it becomes usable
- may hide API selection if the integration is fixed or internally routed

Built-in hosted providers include:

- Copilot
- Codex
- Gemini
- ollama.com
- OpenAI
- OpenRouter

### User-supplied service presets

A user-supplied service preset represents a backend where the user supplies
an endpoint and optionally chooses among multiple APIs.

Properties:

- created through the `/login` flow
- represented in the main provider picker as a normal provider instance once
  configured
- may allow multiple instances
- may allow multiple APIs

User-supplied service presets include:

- Ollama
- Open WebUI
- OpenAI-compatible endpoint
- generic OpenAI-compatible endpoint

## Provider preset semantics

Each provider preset specifies the following semantics.

### Provider identity

The preset identifies which provider the user is choosing.

### Service identity

The preset identifies which backend product or software xi-agent is speaking to.

### API set

The preset defines the set of APIs valid for that backend.

Examples:

- OpenRouter supports OpenAI-compatible
- OpenAI supports OpenAI-compatible
- Codex supports OpenAI Responses
- Gemini supports Gemini native
- Ollama may support Ollama chat API and compatibility APIs
- Open WebUI may support OpenAI-compatible and Ollama chat API

### API selection visibility

A preset either:

- exposes API selection to the user
- or fixes API choice internally

API selection is user-visible only when multiple APIs are valid and meaningful
for the configured backend.

### Endpoint behavior

A preset defines whether the endpoint is:

- predetermined
- user-supplied
- predetermined but overrideable

Predetermined endpoints do not require the user to provide an endpoint during
normal setup.

User-supplied endpoints require an endpoint prompt during setup.

### Authentication mode

A preset defines one of these authentication modes:

- **OAuth login**
- **API key / token**
- **no auth**

The UI presents credentials according to the preset's authentication mode.

### Instance multiplicity

A preset defines whether multiple configured instances are meaningful.

Examples:

- multiple Open WebUI instances may be meaningful
- multiple Ollama instances may be meaningful
- the standard hosted OpenRouter service is represented by a built-in instance

## UI semantics

### Main provider picker

The main provider picker (`/provider`) contains **provider instances**.

It does not contain raw APIs.
It does not contain transport types.

Built-in hosted providers always appear here from the static catalog, even on
a clean install with no config file. Custom backends appear here after the user
creates a provider instance through the login flow.

Each item in the main provider picker is immediately selectable as the active
backend, subject to any missing credentials or required setup.

When no provider instances are configured, the picker shows a placeholder
message ("No providers configured") with a hint to use `/login`. A "Login to
a service…" entry at the bottom of the list opens the login menu.

User-supplied provider instances may also be edited directly from the main
provider picker via a shortcut on the currently highlighted provider entry.

### Login menu

The login menu (`/login`) is the single entry point for connecting to all
services. It lists every service from the static backend catalog (except the
internal test provider).

Selecting a service from the login menu starts the appropriate setup flow:

- **OAuth providers** (Copilot, Codex, Gemini): browser-based OAuth login.
- **API-key providers** (OpenAI, OpenRouter, ollama.com): inline API key
  prompt followed by instance naming.
- **User-supplied presets** (Ollama, Open WebUI, OpenAI-compatible): API
  selection (if applicable), endpoint entry, API key entry (if applicable),
  and instance naming.

The `/login <id>` command also accepts a service id directly, skipping the menu.

### Setup flow

Creating a new provider instance through the login menu follows these steps
depending on the preset:

1. API selection, if user-visible for that preset
2. Endpoint entry, if the preset requires a user-supplied endpoint
3. Credential entry, if required by the preset
4. Instance naming

Examples:

- API key prompt for OpenRouter
- API key prompt for OpenAI
- token prompt for Open WebUI
- OAuth login flow for Copilot, Codex, and Gemini

Selecting a provider instance that lacks required credentials triggers the
credential prompt or login flow needed to complete that provider instance.

## Configuration semantics

### Representation

Configuration stores provider instances as first-class configured entries.

Each provider instance records:

- instance id
- provider preset
- API
- endpoint, if any
- credentials, if any
- model, if any

### Built-in instances

Built-in hosted providers are always available from the static backend catalog.
They do not need to be stored in config to appear in the provider picker.

When the user configures a built-in provider (by selecting a model or providing
credentials), xi-agent persists the instance to config as a normal provider
entry. On subsequent runs, the persisted entry overrides the catalog default.

### Custom instances

Custom/self-hosted backends are represented as normal provider instances in
configuration.

Their distinction is semantic, not structural:

- they are created through the login flow
- they require a user-supplied endpoint and possibly other choices

### Active provider

The active provider is the selected provider instance.

Changing provider means selecting a different provider instance.

## Internal nomenclature

The internal nomenclature follows the domain model above.

### Provider preset

Internally, the system maintains a preset-level definition for each recognized
backend kind.

That definition contains:

- provider/service identity
- API rules
- endpoint behavior
- authentication mode
- default model
- multiplicity semantics
- user-facing labels

### Provider instance

Internally, the system maintains provider instances as the configured backend
entries the user can select.

A provider instance references exactly one preset and exactly one selected API.

### API type

Internally, the system represents API choice independently of provider preset
and provider instance.

An API type is a protocol concept, not a provider concept.

### Endpoint field

Internally, the configured endpoint is the URL used to reach the selected API
for the provider instance.

## Backend-specific semantics

### OpenRouter

OpenRouter is a built-in hosted provider.

Semantics:

- provider: OpenRouter
- service: OpenRouter
- API: OpenAI-compatible
- endpoint: predetermined
- authentication: API key
- UI presence: built-in provider instance in the main provider picker

Selecting OpenRouter without an API key prompts for an API key.

### OpenAI

OpenAI is a built-in hosted provider.

Semantics:

- provider: OpenAI
- service: OpenAI API
- API: OpenAI-compatible
- endpoint: predetermined
- authentication: API key
- UI presence: built-in provider instance in the main provider picker

### Copilot

Copilot is a built-in hosted provider.

Semantics:

- provider: GitHub
- service: Copilot
- API: backend/model dependent and internally constrained
- endpoint: predetermined by the integration
- authentication: OAuth login
- UI presence: built-in provider instance in the main provider picker

Provider selection and API behavior are distinct for Copilot. The user selects
Copilot as a provider instance, while xi-agent applies backend-specific routing
rules internally.

### Codex

Codex is a built-in hosted provider.

Semantics:

- provider: OpenAI
- service: Codex / chatgpt.com backend
- API: OpenAI Responses
- endpoint: predetermined
- authentication: OAuth login
- UI presence: built-in provider instance in the main provider picker

### Gemini

Gemini is a built-in hosted provider.

Semantics:

- provider: Google
- service: Gemini / Cloud Code Assist
- API: Gemini native
- endpoint: predetermined
- authentication: OAuth login
- UI presence: built-in provider instance in the main provider picker

### Ollama

Ollama is a user-supplied service backend.

Semantics:

- provider: the operator of the selected Ollama deployment
- service: Ollama
- API: selectable from the APIs supported by the preset
- endpoint: user-supplied
- authentication: usually none
- UI presence: created via add-provider, then appears as a provider instance in
  the main provider picker

### Open WebUI

Open WebUI is a user-supplied service backend.

Semantics:

- provider: the operator of the selected Open WebUI deployment
- service: Open WebUI
- API: selectable from the APIs supported by the preset
- endpoint: user-supplied
- authentication: token/API key
- UI presence: created via add-provider, then appears as a provider instance in
  the main provider picker

### Generic OpenAI-compatible endpoint

A generic OpenAI-compatible endpoint is a user-supplied service backend.

Semantics:

- provider: the operator of the selected endpoint
- service: unspecified but OpenAI-compatible
- API: OpenAI-compatible
- endpoint: user-supplied
- authentication: API key or token
- UI presence: created via add-provider, then appears as a provider instance in
  the main provider picker

## Invariants

The following invariants define the provider model.

1. The main provider picker contains provider instances.
2. A provider instance references exactly one provider preset.
3. A provider instance uses exactly one API at a time.
4. API choice is determined by preset rules.
5. Endpoint prompting occurs only for provider instances whose preset requires a
   user-supplied endpoint.
6. Credential prompting occurs only according to the preset's authentication
   mode.
7. Built-in hosted providers are always available in the main provider picker
   from the static catalog, without requiring config entries.
8. Custom/self-hosted backends become selectable only after creation through
   the login flow.
9. Built-in and custom backends share the same provider-instance model once
   configured.
10. Provider identity, service identity, API, endpoint, and credentials are
    distinct concepts even when a backend fixes some of them implicitly.

## Relationship summary

```text
Provider preset
  -> defines provider identity
  -> defines service identity
  -> defines allowed APIs
  -> defines endpoint behavior
  -> defines authentication mode
  -> defines default model
  -> defines whether built-in or user-created instances are valid

Provider instance
  -> selects one provider preset
  -> selects one API
  -> stores endpoint if needed
  -> stores credentials if needed
  -> stores selected/default model
  -> appears in the main provider picker
```
