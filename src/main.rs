use std::convert::Infallible;
use std::net::SocketAddr;
use std::collections::HashMap;
use hyper::{Body, Client, Request, Response, Server, StatusCode, Uri};
use hyper::client::HttpConnector;
use hyper::service::{make_service_fn, service_fn};
use std::sync::Arc;
use hyper::upgrade::Upgraded;
use tokio::net::TcpStream;
use futures_util::future::try_join;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Configuration for our path-based routing proxy
struct ProxyConfig {
    // Maps path prefixes to backend services
    route_map: HashMap<String, String>,
    // Default backend if no path matches
    default_backend: Option<String>,
}

impl ProxyConfig {
    fn new() -> Self {
        ProxyConfig {
            route_map: HashMap::new(),
            default_backend: None,
        }
    }

    // Add a new route mapping
    fn add_route(&mut self, path_prefix: &str, backend: &str) {
        self.route_map.insert(path_prefix.to_string(), backend.to_string());
    }

    // Set the default backend
    fn set_default_backend(&mut self, backend: &str) {
        self.default_backend = Some(backend.to_string());
    }

    // Find the appropriate backend for a given path
    fn get_backend_for_path(&self, path: &str) -> Option<String> {
        // First, try to find the longest matching prefix
        let mut matches: Vec<(&String, &String)> = self.route_map
            .iter()
            .filter(|(prefix, _)| path.starts_with(prefix.as_str()))
            .collect();
        
        // Sort by prefix length (longest first)
        matches.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()));
        
        // Return the backend with the longest matching prefix, or the default backend
        matches.first()
            .map(|(_, backend)| backend.to_string())
            .or_else(|| self.default_backend.clone())
    }
}

// Add this new function to handle WebSocket connections
async fn proxy_websocket(
    upgraded: Upgraded,
    backend_uri: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Connect to the backend
    let stream = TcpStream::connect(backend_uri).await?;
    let (mut client_rd, mut client_wr) = tokio::io::split(upgraded);
    let (mut server_rd, mut server_wr) = tokio::io::split(stream);

    // Copy bidirectionally
    let client_to_server = async {
        tokio::io::copy(&mut client_rd, &mut server_wr).await?;
        server_wr.shutdown().await
    };

    let server_to_client = async {
        tokio::io::copy(&mut server_rd, &mut client_wr).await?;
        client_wr.shutdown().await
    };

    try_join(client_to_server, server_to_client).await?;
    Ok(())
}

async fn proxy_request(
    client: Client<HttpConnector>,
    req: Request<Body>,
    config: Arc<ProxyConfig>,
) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path();
    
    // Get the backend for this path
    if let Some(backend) = config.get_backend_for_path(path) {
        println!("Routing request for path '{}' to backend: {}", path, backend);

        // Check if this is a WebSocket upgrade request
        if req.headers().contains_key(hyper::header::UPGRADE) {
            println!("Upgrading connection to WebSocket");
            
            // Parse the backend URL to get host and port
            let backend_uri = backend.replace("http://", "");
            let backend_uri = backend_uri.replace("https://", "");
            
            // Create upgrade response
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            
            // Copy upgrade headers from client request to response
            if let Some(connection) = req.headers().get("connection") {
                response.headers_mut().insert("connection", connection.clone());
            }
            if let Some(upgrade) = req.headers().get("upgrade") {
                response.headers_mut().insert("upgrade", upgrade.clone());
            }
            if let Some(key) = req.headers().get("sec-websocket-key") {
                response.headers_mut().insert("sec-websocket-key", key.clone());
            }
            if let Some(version) = req.headers().get("sec-websocket-version") {
                response.headers_mut().insert("sec-websocket-version", version.clone());
            }
            
            // Spawn a task to handle the WebSocket connection
            tokio::task::spawn(async move {
                match hyper::upgrade::on(req).await {
                    Ok(upgraded) => {
                        if let Err(e) = proxy_websocket(upgraded, backend_uri).await {
                            eprintln!("WebSocket proxy error: {}", e);
                        }
                    }
                    Err(e) => eprintln!("WebSocket upgrade error: {}", e),
                }
            });

            Ok(response)
        } else {
            // Handle regular HTTP request as before
            let (parts, body) = req.into_parts();
            let uri_string = format!("{}{}", backend, parts.uri.path_and_query().map(|x| x.as_str()).unwrap_or(""));
            let uri: Uri = uri_string.parse().unwrap();
            
            let mut new_req = Request::builder()
                .method(parts.method)
                .uri(uri);
            
            for (name, value) in parts.headers.iter() {
                new_req = new_req.header(name, value);
            }
            
            new_req = new_req.header("X-Forwarded-For", 
                parts.headers.get("host").unwrap_or(&"unknown".parse().unwrap()));
            new_req = new_req.header("X-Forwarded-Proto", "http");
            
            let new_req = new_req.body(body).unwrap();
            client.request(new_req).await
        }
    } else {
        // No matching backend found
        let mut response = Response::new(Body::from("No service configured for this path"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        Ok(response)
    }
}

#[tokio::main]
async fn main() {
    // Create client to send requests to backends
    let client = Client::new();
    
    // Configure path-based routing
    let mut config = ProxyConfig::new();
    
    // Add route mappings for different microservices
    config.add_route("/api/users", "http://user-service:8081");
    config.add_route("/api/products", "http://product-service:8082");
    config.add_route("/api/orders", "http://order-service:8083");
    config.add_route("/auth", "http://auth-service:8084");
    
    // Nested paths - more specific paths should be added first
    config.add_route("/api/products/inventory", "http://inventory-service:8085");
    
    // Set a default backend for unmatched paths
    config.set_default_backend("http://localhost:5173");
    
    // Print route configuration before wrapping in Arc
    println!("Configured routes:");
    for (path, backend) in &config.route_map {
        println!("  {} -> {}", path, backend);
    }
    if let Some(backend) = &config.default_backend {
        println!("  Default -> {}", backend);
    }
    
    // Wrap config in Arc for sharing across requests
    let config = Arc::new(config);
    
    // Define the address to bind to
    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    
    println!("Path-based reverse proxy listening on http://{}", addr);
    
    // Create a service function to handle each request
    let make_svc = make_service_fn(move |_conn| {
        let client = client.clone();
        let config = config.clone();
        
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                let client = client.clone();
                let config = config.clone();
                
                async move {
                    proxy_request(client, req, config).await
                }
            }))
        }
    });
    
    // Start the server
    let server = Server::bind(&addr).serve(make_svc);
    
    // Run the server
    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
