use std::convert::Infallible;
use std::net::SocketAddr;
use std::collections::HashMap;
use hyper::{Body, Client, Request, Response, Server, StatusCode, Uri};
use hyper::client::HttpConnector;
use hyper::service::{make_service_fn, service_fn};
use std::sync::Arc;

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

async fn proxy_request(
    client: Client<HttpConnector>,
    req: Request<Body>,
    config: Arc<ProxyConfig>,
) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path();
    
    // Get the backend for this path
    if let Some(backend) = config.get_backend_for_path(path) {
        println!("Routing request for path '{}' to backend: {}", path, backend);
        
        // Clone the request parts
        let (parts, body) = req.into_parts();
        
        // Construct the new URI
        let uri_string = format!("{}{}", backend, parts.uri.path_and_query().map(|x| x.as_str()).unwrap_or(""));
        let uri: Uri = uri_string.parse().unwrap();
        
        // Create a new request with the same method, headers, and body
        let mut new_req = Request::builder()
            .method(parts.method)
            .uri(uri);
        
        // Copy headers from the original request
        for (name, value) in parts.headers.iter() {
            new_req = new_req.header(name, value);
        }
        
        // Add proxy-related headers
        new_req = new_req.header("X-Forwarded-For", 
            parts.headers.get("host").unwrap_or(&"unknown".parse().unwrap()));
        new_req = new_req.header("X-Forwarded-Proto", "http");
        
        // Build the new request
        let new_req = new_req.body(body).unwrap();
        
        // Send the request to the backend
        client.request(new_req).await
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
    config.set_default_backend("http://frontend-service:8080");
    
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
