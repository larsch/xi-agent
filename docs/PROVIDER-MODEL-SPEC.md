# Provider Model Specification

## Purpose

This document defines tau's backend model.

It specifies:

- the domain concepts tau uses for backend integrations
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

A **service** is the backend product or software tau talks to.

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

An **API** is the protocol surface tau uses to communicate with a service.

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
- model selection semantics exposed to tau

### Endpoint

An **endpoint** is the concrete URL where tau reaches an API.

Examples:

- `https://openrouter.ai/api/v1`
- `https://api.openai.com/v1`
- `http://localhost:11434`
- `https://my-webui.example.com/api`

An endpoint is either:

- **predetermined** by the provider/service integration
- **user-supplied** during provider instance setup

### Provider preset

A **provider preset** is tau's built-in definition of a recognized backend kind.

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

A **provider instance** is one configured, selectable backend entry in tau.

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

- a **built-in instance**, synthesized by tau from a built-in preset
- a **custom instance**, created by the user from a preset that allows user configuration

## Backend classes

Tau supports two backend classes.

### Built-in hosted providers

A built-in hosted provider has a recognized provider identity, a recognized
service identity, and a predetermined endpoint family.

Properties:

- appears as a first-class provider instance in the main provider picker
- does not require the user to invent a custom instance just to use the normal
  hosted service
- may still require credentials before it becomes usable
- may hide API selection if the integration is fixed or internally routed

Built-in hosted providers include:

- OpenRouter
- OpenAI
- Copilot
- Codex
- Gemini

### User-supplied service instances

A user-supplied service instance represents a backend where the user supplies
an endpoint and optionally chooses among multiple APIs.

Properties:

- created through the add-provider flow
- represented in the main provider picker as a normal provider instance once
  configured
- may allow multiple instances
- may allow multiple APIs

User-supplied service instances include:

- Ollama
- Open WebUI
- generic OpenAI-compatible endpoint

## Provider preset semantics

Each provider preset specifies the following semantics.

### Provider identity

The preset identifies which provider the user is choosing.

### Service identity

The preset identifies which backend product or software tau is speaking to.

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

The main provider picker contains **provider instances**.

It does not contain raw APIs.
It does not contain transport types.
It does not directly contain provider presets unless those presets are realized
as provider instances.

Each item in the main provider picker is immediately selectable as the active
backend, subject to any missing credentials or required setup.

Built-in hosted providers appear here as built-in provider instances.
Custom backends appear here after the user creates a provider instance for
them.

User-supplied provider instances may also be edited directly from the main
provider picker via a shortcut on the currently highlighted provider entry.
That edit action is attached to the existing provider row rather than shown as
its own separate picker item.

### Add-provider flow

The add-provider flow creates a **new provider instance**.

The flow is used for presets that support user-created instances.

The flow consists of these conceptual steps:

1. instance naming
2. provider preset selection
3. API selection, if user-visible for that preset
4. endpoint entry, if the preset requires a user-supplied endpoint
5. credential entry, if required by the preset

The result of the flow is a provider instance.

### Provider preset selection

The provider preset selection step chooses what kind of backend the new
provider instance will represent.

This selection is about backend identity and setup semantics, not about the
active provider instance yet.

### API selection

The API selection step chooses which API the provider instance will use.

This step is shown only when the selected preset exposes multiple valid APIs.

### Endpoint prompt

The endpoint prompt collects the provider instance's endpoint.

The endpoint prompt is shown only when the selected preset requires a
user-supplied endpoint.

Examples:

- Ollama endpoint
- Open WebUI URL
- generic OpenAI-compatible endpoint URL

### Credential prompt

The credential prompt collects the provider instance's required credentials.

The prompt type is determined by the preset's authentication mode.

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

Built-in hosted providers are represented as normal provider instances in
configuration.

Their distinction is semantic, not structural:

- they are synthesized from built-in presets
- they represent the standard hosted service
- they may start with incomplete credentials and become usable after login or
  API key entry

### Custom instances

Custom/self-hosted backends are also represented as normal provider instances
in configuration.

Their distinction is semantic, not structural:

- they are created through the add-provider flow
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
Copilot as a provider instance, while tau applies backend-specific routing
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
7. Built-in hosted providers appear as built-in provider instances in the main
   provider picker.
8. Custom/self-hosted backends become selectable only after creation of a
   provider instance.
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
