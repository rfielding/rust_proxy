# Rust Reverse Proxy with Hyper

This project implements a path-based reverse proxy using Hyper, a fast and correct HTTP implementation written in Rust. The proxy supports both regular HTTP requests and WebSocket connections.

## What is Hyper?

Hyper is a powerful, low-level HTTP library for Rust that provides:
- Asynchronous design using Tokio
- Both client and server implementations
- HTTP/1 and HTTP/2 support
- High performance and safety guarantees
- Extensible architecture

## Key Components in Our Implementation

### 1. Server Setup 