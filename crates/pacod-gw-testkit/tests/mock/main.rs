//! Tests of the mock providers: every emulated API in both modes, request capture, every
//! scripted behaviour, determinism, and the control endpoints; and of the tool servers and the
//! agent `gw-mock` serves next to them.

mod anthropic;
mod behaviors;
mod capture;
mod control;
mod emulate;
mod gemini;
mod ollama;
mod openai;
mod path_options;
mod support;
mod tools_and_agents;
