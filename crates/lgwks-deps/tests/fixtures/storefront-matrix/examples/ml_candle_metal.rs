use lgwks_deps::candle_transformers;

fn main() {
    let _models = std::any::type_name::<candle_transformers::models::quantized_llama::ModelWeights>();
}
