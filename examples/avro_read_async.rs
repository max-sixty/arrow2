use std::sync::Arc;

use futures::pin_mut;
use futures::StreamExt;
use tokio::fs::File;
use tokio_util::compat::*;

use arrow2::error::Result;
use arrow2::io::avro::read::{decompress_block, deserialize};
use arrow2::io::avro::read_async::*;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    use std::env;
    let args: Vec<String> = env::args().collect();

    let file_path = &args[1];

    let mut reader = File::open(file_path).await?.compat();

    let (avro_schemas, schema, compression, marker) = read_metadata(&mut reader).await?;
    let schema = Arc::new(schema);
    let avro_schemas = Arc::new(avro_schemas);

    let blocks = block_stream(&mut reader, marker).await;

    pin_mut!(blocks);
    while let Some((mut block, rows)) = blocks.next().await.transpose()? {
        // the content here is blocking. In general this should run on spawn_blocking
        let schema = schema.clone();
        let avro_schemas = avro_schemas.clone();
        let handle = tokio::task::spawn_blocking(move || {
            let mut decompressed = vec![];
            decompress_block(&mut block, &mut decompressed, compression)?;
            deserialize(&decompressed, rows, schema, &avro_schemas)
        });
        let batch = handle.await.unwrap()?;
        assert!(batch.num_rows() > 0);
    }

    Ok(())
}
