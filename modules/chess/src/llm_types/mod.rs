// NOTE at some point these should be published as a crate
// and should also contain convenience functions

use serde::{Deserialize, Serialize};

mod llama_cpp;
mod open_ai;

#[allow(unused_imports)]
pub use llama_cpp::*;
#[allow(unused_imports)]
pub use open_ai::*;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MainAction {
    NewModel(NewModel),
    ListModels,
    Allow(Allow),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum LlmAction {
    Config(LlmConfig),
    Chat(Chat),
    Embedding(Embedding),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct NewModel {
    pub name: String,
    pub config: LlmConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Chat {
    pub prompt: String,
    pub params: ChatParams,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChatParams {
    pub max_tokens: Option<u64>, // aka n_predict
    pub stops: Option<Vec<String>>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub frequency_penalty: Option<f64>,
    // pub logit_bias: Option<HashMap<String, String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChatResponse {
    pub content: String,
    // pub usage: Option<OpenAiChatCompletionUsage>,
    // retries: u64,
    // ms: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Embedding {
    pub model: String,
    pub texts: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Allow {
    pub model: String,
    pub process: String, // example:package:publisher
}

// | "gpt-4"
// | "gpt-4-0613"
// | "gpt-4-32k"
// | "gpt-4-32k-0613"
// | "gpt-3.5-turbo"
// | "gpt-3.5-turbo-0613"
// | "gpt-3.5-turbo-16k"
// | "gpt-3.5-turbo-16k-0613"
// | "gpt-4-1106-preview"
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OpenAiConfig {
    pub api_key: String,
    pub chat_model: String,
    pub embedding_model: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LlamaCppConfig {
    pub url: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum LlmConfig { // TODO/NOTE there will probably be a *lot* more config options added here later
    OpenAi(OpenAiConfig),
    LlamaCpp(LlamaCppConfig),
}
