use std::net::ToSocketAddrs;

#[tokio::main]
async fn main() {
    println!("Testing DNS seed discovery...");
    
    let seed_servers = vec![
        "seeder1.tondid.net",
        "seeder2.tondid.net", 
        "seeder3.tondid.net",
        "dnsseed.tondi.org",
        "seed.tondi.org"
    ];
    
    for seed_server in seed_servers {
        println!("\nTesting: {}", seed_server);
        
        // Test basic DNS resolution
        match (seed_server, 16111u16).to_socket_addrs() {
            Ok(addrs) => {
                let addrs: Vec<_> = addrs.collect();
                println!("  DNS resolution: {} addresses found", addrs.len());
                for addr in addrs.iter().take(5) {
                    println!("    - {}:{}", addr.ip(), addr.port());
                }
            }
            Err(e) => {
                println!("  DNS resolution failed: {}", e);
            }
        }
    }
}
