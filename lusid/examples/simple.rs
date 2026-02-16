use lusid::{
    create_store,
    operation::apply,
    plan::{PlanId, plan},
};
use lusid_params::ParamValues;
use rimu::SourceId;
use serde::Serialize;
use std::env;

#[derive(Serialize)]
struct ExampleParams {
    pub whatever: bool,
}

#[tokio::main]
async fn main() {
    let mut store = create_store();

    let path = env::current_dir().expect("Failed to get env::current_dir()");
    let plan_id = PlanId::Path(path.join("examples/simple.lusid"));

    let params = ParamValues::from_type(ExampleParams { whatever: true }, SourceId::empty())
        .expect("Failed to create params");

    let operation = plan(plan_id, Some(params), &mut store)
        .await
        .expect("Failed to plan");

    apply(operation).await.expect("Failed to apply");
}
