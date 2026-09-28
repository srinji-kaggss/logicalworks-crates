use lgwks_deps::{candle_core, candle_nn, candle_transformers};

fn main() {
    let _tensor = std::any::type_name::<candle_core::Tensor>();
    let _variables = std::any::type_name::<candle_nn::VarMap>();
    let _models = std::any::type_name::<candle_transformers::models::quantized_llama::ModelWeights>();
}
