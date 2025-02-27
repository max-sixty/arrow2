use std::sync::Arc;

use avro_rs::Codec;

use futures::pin_mut;
use futures::StreamExt;

use arrow2::error::Result;
use arrow2::io::avro::read_async::*;

use super::read::write;

async fn test(codec: Codec) -> Result<()> {
    let (data, expected) = write(codec).unwrap();

    let mut reader = &mut &data[..];

    let (_, schema, _, marker) = read_metadata(&mut reader).await?;
    let schema = Arc::new(schema);

    assert_eq!(schema.as_ref(), expected.schema().as_ref());

    let blocks = block_stream(&mut reader, marker).await;

    pin_mut!(blocks);
    while let Some((block, rows)) = blocks.next().await.transpose()? {
        assert!(rows > 0 || block.is_empty())
    }
    Ok(())
}

#[tokio::test]
async fn read_without_codec() -> Result<()> {
    test(Codec::Null).await
}

#[tokio::test]
async fn read_deflate() -> Result<()> {
    test(Codec::Deflate).await
}

#[tokio::test]
async fn read_snappy() -> Result<()> {
    test(Codec::Snappy).await
}
