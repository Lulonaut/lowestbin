use convert_case::{Case, Casing};
use dashmap::DashMap;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use hyper::service::{make_service_fn, service_fn};
use hyper::{header, Body, Method, Request, Response, Server};
use nbt::from_gzip_reader;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::convert::Infallible;
use std::io::Cursor;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

static mut DATA: String = String::new();

#[derive(Deserialize, Debug)]
pub struct PartialNbt {
    pub i: Vec<PartialNbtElement>,
}

#[derive(Deserialize, Debug)]
pub struct PartialNbtElement {
    #[serde(rename = "Count")]
    pub count: i64,
    pub tag: PartialTag,
}
#[derive(Deserialize, Debug)]
pub struct PartialTag {
    #[serde(rename = "ExtraAttributes")]
    pub extra_attributes: PartialExtraAttr,
    pub display: DisplayInfo,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Pet {
    #[serde(rename = "type")]
    pub pet_type: String,

    #[serde(rename = "tier")]
    pub tier: String,
}

#[derive(Deserialize, Debug)]
pub struct PartialExtraAttr {
    pub id: String,
    #[serde(rename = "petInfo")]
    pub pet: Option<String>,
    pub enchantments: Option<HashMap<String, i32>>,
    pub potion: Option<String>,
    pub potion_level: Option<i16>,
    pub anvil_uses: Option<i16>,
    pub enhanced: Option<bool>,
    pub runes: Option<HashMap<String, i32>>,
}

#[derive(Deserialize, Debug)]
pub struct DisplayInfo {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Lore")]
    pub lore: Vec<String>,
}

#[tokio::main]
async fn main() {
    //refresh data every 2 mins
    let mut interval = tokio::time::interval(Duration::from_secs(120));
    tokio::spawn(async move {
        loop {
            interval.tick().await;
            tokio::spawn(update());
        }
    });

    start_server().await;
}

async fn update() {
    let now = Instant::now();
    let client = reqwest::Client::builder()
        .gzip(true)
        .brotli(true)
        .build()
        .unwrap();

    let inital_response = json_request(0, &client).await;
    let page_count_response = inital_response.get("totalPages");
    let page_count: i64;
    match page_count_response {
        None => {
            eprintln!("Hypixel API didn't return any info on the total amount of pages");
            return;
        }
        Some(x) => page_count = x.as_i64().unwrap(),
    }
    let mut futures = FuturesUnordered::new();

    for page in 0..page_count {
        let future = json_request(page, &client);
        futures.push(future);
    }

    let collected_prices: DashMap<String, i64> = DashMap::new();
    while let Some(response) = futures.next().await {
        let auctions_array = response.get("auctions");
        for auction_entry in auctions_array.unwrap().as_array().into_iter().into_iter() {
            for auction in auction_entry {
                if auction.get("bin").is_none() {
                    continue;
                }
                let price = auction.get("starting_bid").unwrap().as_i64().unwrap();
                let item_bytes = auction
                    .get("item_bytes")
                    .unwrap()
                    .to_string()
                    .replace("\"", "");
                let item_bytes = base64::decode(item_bytes).unwrap();
                let nbt: PartialNbt = from_gzip_reader(Cursor::new(item_bytes)).unwrap();
                let final_id: String;
                //pets
                if nbt.i[0].tag.extra_attributes.pet.is_some() {
                    let full_name = nbt.i[0].tag.display.name.split(']').collect::<Vec<&str>>();
                    if full_name.len() == 2 {
                        let full_name = full_name[1];
                        let mut name = remove_color_codes(full_name.to_string());
                        name = name.replace(" ", "_").to_uppercase();

                        //getting the rarity
                        let lore = &nbt.i[0].tag.display.lore.last().unwrap();
                        let rarity = remove_color_codes(lore.to_string());
                        //assembling the final String
                        final_id = format!("PET:{}:{}", name, rarity);
                    } else {
                        //pet items (I think)
                        final_id = remove_color_codes(full_name[0].to_string());
                    }
                //enchanted books
                } else if nbt.i[0].tag.extra_attributes.id.contains("ENCHANTED_BOOK") {
                    let enchants = &nbt.i[0].tag.extra_attributes.enchantments;
                    //just a normal enchanted book, don't care about that
                    if enchants.is_none() {
                        continue;
                    }
                    //ignore books with multiple enchants
                    if enchants.as_ref().unwrap().len() > 1 {
                        continue;
                    }

                    let name = remove_color_codes(nbt.i[0].tag.display.lore[0].to_string());
                    final_id = name.to_case(Case::Snake).to_uppercase();
                //normal items
                } else {
                    final_id = nbt.i[0].tag.extra_attributes.id.to_string();
                }
                //compare this price to the one already collected
                if collected_prices.contains_key(&final_id) {
                    if collected_prices.get(&final_id).unwrap().value() > &price {
                        collected_prices.insert(final_id, price);
                    }
                } else {
                    collected_prices.insert(final_id, price);
                }
            }
        }
    }
    unsafe {
        DATA = format!("{:?}", collected_prices);
    }
    println!("parsed all auctions in {}ms", now.elapsed().as_millis());
}

async fn json_request(page: i64, client: &Client) -> Value {
    let url = format!("https://api.hypixel.net/skyblock/auctions?page={}", page);
    let resp = client.get(url).send().await;
    if resp.is_err() {
        eprintln!("Error while sending Request: {}", resp.err().unwrap());
        return Value::Null;
    }
    let json = serde_json::from_str(resp.unwrap().text().await.unwrap().as_str());
    if json.is_err() {
        eprintln!("Error while turning request into JSON");
        return Value::Null;
    }
    let json: Value = json.unwrap();
    if !json.get("success").unwrap().as_bool().unwrap() {
        eprintln!(
            "Hypixel API Error: {}",
            json.get("cause").unwrap().as_str().unwrap()
        );
        return Value::Null;
    }
    json
}

fn remove_color_codes(str: String) -> String {
    let mut cleaned = String::new();
    for char in str.chars() {
        if char.is_alphabetic() || char.is_whitespace() {
            cleaned.push(char);
        }
    }
    cleaned = cleaned.trim().to_string();
    while cleaned.chars().next().unwrap().is_lowercase() {
        cleaned.remove(0);
    }
    cleaned
}

async fn handle(request: Request<Body>) -> Result<Response<Body>, Infallible> {
    if request.method() == Method::GET
        && (request.uri().path() == "/lowestbins" || request.uri().path() == "/lowestbins.json")
    {
        //mutable statics are unsafe because they can be modified by multiple threads, this does not happen here.
        unsafe {
            Ok(Response::builder()
                .header(header::CONTENT_TYPE, "application/json")
                .header("Access-Control-Allow-Origin", "*")
                .body(Body::from(DATA.as_str()))
                .unwrap())
        }
    } else {
        Ok(Response::builder()
            .status(404)
            .header("Access-Control-Allow-Origin", "*")
            .body(Body::from(
                "Not found. Available Endpoints are: /lowestbins, /lowestbins.json",
            ))
            .unwrap())
    }
}

async fn start_server() {
    let addr = SocketAddr::from(([127, 0, 0, 1], 80));

    let make_service = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle)) });

    let server = Server::bind(&addr).serve(make_service);

    println!("Listening on http://{}", addr);

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
