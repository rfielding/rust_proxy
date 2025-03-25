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

- Requests to `/path1/*` route to backend1
- Requests to `/path2/*` route to backend2
- All other requests route to the default backend

## Key Rust Concepts Demonstrated

- Async/await with Tokio
- Error handling with Result
- Thread-safe sharing with Arc
- Ownership and borrowing in network handlers
- Command-line argument parsing
- HashMap for configuration storage
- Trait implementations for custom types