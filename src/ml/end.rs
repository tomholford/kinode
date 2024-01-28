use crate::ml::mixtral_sharded::EndModel;
use crate::ml::mixtral_sharded::Model;
use anyhow::Result;
use candle_core::DType;
use candle_transformers::generation::LogitsProcessor;

use crate::ml::device;
use crate::ml::util::create_paths;
use crate::ml::util::load_model;
use crate::ml::util::Args;
use candle_core::{Device, Tensor};

use super::model::Model;

pub struct EndProcessor {
    model: Option<EndModel>,
    model_path: std::path::PathBuf,
    device: Device,
    iteration: usize,
    kv_caches: Option<Vec<(Tensor, Tensor)>>,

    logits_processor: LogitsProcessor,
    shard_num: usize,
}
// TODO: Repeat penalty and repeat last n?

impl EndProcessor {
    pub fn new(args: &Args, shard_num: usize) -> Result<Self> {
        let paths = create_paths(&args);
        let model_path = paths[shard_num].clone();
        println!("Loading model from {:?}", model_path);
        let device = device::device(args.cpu)?;
        let logits_processor = LogitsProcessor::new(args.seed, args.temperature, args.top_p);
        let result = Self {
            model: None,
            model_path,
            logits_processor,
            device,
            iteration: Default::default(),
            shard_num,
            kv_caches: None,
        };
        Ok(result)
    }
}

impl Model for EndProcessor {
    fn load_model_if_not_loaded(&mut self) -> Result<()> {
        let Model::End(model) = load_model(&self.device, &self.model_path, self.shard_num)? else {
            panic!("Model is not end")
        };
        self.model = Some(model);
        if let Some(kv_caches) = self.kv_caches.take() {
            if let Some(model) = &mut self.model {
                model.set_kv_caches(kv_caches);
                println!("Size of kv_caches: {}", model.get_kv_caches().len());
            }
        }
        Ok(())
    }

    fn unload_model(&mut self) {
        if let Some(model) = &self.model {
            self.kv_caches = Some(model.get_kv_caches());
        }
        self.model = None;
    }

    #[allow(unused)] // TODO: Luc
    fn clear(&mut self) {
        self.iteration = 0;
    }

    fn forward(&mut self, activation: &Tensor, start_pos: usize) -> Result<u32> {
        if self.model.is_none() {
            if let Err(e) = self.load_model_if_not_loaded() {
                println!("Error loading model: {:?}", e);
            }
        }
        if let Some(model) = &mut self.model {
            // TODO: Luc: why do the squeeze here and not in the model?
            let logits = model
                .forward(activation, start_pos)?
                .squeeze(0)?
                .squeeze(0)?
                .to_dtype(DType::F32)?;
            let next_token = self.logits_processor.sample(&logits)?;
            println!("Next token is {}", next_token);
            return Ok(next_token);
        }
        panic!("Something went terribly wrong")
    }
}
