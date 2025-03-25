use std::convert::Infallible;
use std::net::SocketAddr;
use std::collections::HashMap;
use hyper::{Body, Client, Request, Response, Server, StatusCode, Uri};
use hyper::client::HttpConnector;
use hyper::service::{make_service_fn, service_fn};
use std::sync::Arc;
use hyper::upgrade::Upgraded;
use tokio::net::TcpStream;
use sha1::{Sha1, Digest};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

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

fn generate_websocket_accept(key: &str) -> String {
    let mut sha1 = Sha1::new();
    sha1.update(format!("{}258EAFA5-E914-47DA-95CA-C5AB0DC85B11", key));
    BASE64.encode(sha1.finalize())
}

async fn proxy_websocket(
    upgraded: Upgraded,
    backend_uri: String,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let uri_str = backend_uri.replace("http://", "");
    let (host, port) = if let Some(idx) = uri_str.find(':') {
        (&uri_str[..idx], uri_str[idx+1..].parse::<u16>().unwrap_or(80))
    } else {
        (uri_str.as_str(), 80)
    };

    let stream = TcpStream::connect((host, port)).await?;
    let (mut client_rd, mut client_wr) = tokio::io::split(upgraded);
    let (mut server_rd, mut server_wr) = tokio::io::split(stream);

    let client_to_server = async {
        tokio::io::copy(&mut client_rd, &mut server_wr).await?;
        Ok::<_, std::io::Error>(())
    };

    let server_to_client = async {
        tokio::io::copy(&mut server_rd, &mut client_wr).await?;
        Ok::<_, std::io::Error>(())
    };

    tokio::try_join!(client_to_server, server_to_client)?;
    Ok(())
}

async fn proxy_request(
    client: Client<HttpConnector>,
    req: Request<Body>,
    config: Arc<ProxyConfig>,
) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path();
    
    if let Some(backend) = config.get_backend_for_path(path) {
        println!("Routing request for path '{}' to backend: {}", path, backend);

        // Check for WebSocket upgrade without consuming the request
        if req.headers().contains_key(hyper::header::UPGRADE) {
            println!("WebSocket upgrade request detected");
            
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
            response.headers_mut().append(
                hyper::header::CONNECTION,
                hyper::header::HeaderValue::from_static("upgrade")
            );
            response.headers_mut().append(
                hyper::header::UPGRADE,
                hyper::header::HeaderValue::from_static("websocket")
            );
            
            // Generate WebSocket accept key if provided
            if let Some(key) = req.headers().get("Sec-WebSocket-Key") {
                let accept = generate_websocket_accept(key.to_str().unwrap());
                response.headers_mut().append(
                    "Sec-WebSocket-Accept",
                    hyper::header::HeaderValue::from_str(&accept).unwrap()
                );
            }

            // Spawn a task to handle the WebSocket connection
            let backend = backend.clone();
            tokio::spawn(async move {
                match hyper::upgrade::on(req).await {
                    Ok(upgraded) => {
                        if let Err(e) = proxy_websocket(upgraded, backend).await {
                            eprintln!("WebSocket proxy error: {}", e);
                        }
                    }
                    Err(e) => eprintln!("WebSocket upgrade error: {}", e),
                }
            });

            return Ok(response);
        }

        // Handle regular HTTP request
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
    } else {
        let mut response = Response::new(Body::from("No service configured for this path"));
        *response.status_mut() = StatusCode::NOT_FOUND;
        Ok(response)
    }
}

#[tokio::main]
async fn main() {
    // Get command line arguments
    let args: Vec<String> = std::env::args().collect();
    
    // Check if --fwd flag is present
    let fwd_index = args.iter().position(|arg| arg == "--fwd");
    
    if fwd_index.is_none() {
        eprintln!("Usage: {} --fwd [path url]... [default url]", args[0]);
        eprintln!("Example: {} --fwd /auth http://docker:9932 default http://localhost:5173", args[0]);
        std::process::exit(1);
    }
    
    // Create client to send requests to backends
    let client = Client::new();
    
    // Configure path-based routing
    let mut config = ProxyConfig::new();
    
    // Process forwarding rules
    let fwd_args = &args[fwd_index.unwrap() + 1..];
    let mut i = 0;
    while i < fwd_args.len() {
        if i + 1 >= fwd_args.len() {
            eprintln!("Error: Each path must have a corresponding URL");
            std::process::exit(1);
        }
        
        let path = &fwd_args[i];
        let url = &fwd_args[i + 1];
        
        if path == "default" {
            config.set_default_backend(url);
            println!("Setting default backend to: {}", url);
        } else {
            config.add_route(path, url);
            println!("Adding route: {} -> {}", path, url);
        }
        
        i += 2;
    }
    
    // Wrap config in Arc for sharing across requests
    let config = Arc::new(config);
    
    // Define the address to bind to
    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    
    println!("\nPath-based reverse proxy listening on http://{}", addr);
    
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
