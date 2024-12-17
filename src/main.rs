use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::{Builder, JoinHandle};
use std::time::Duration;
use crossbeam_channel::{Receiver, RecvError};
use solana_streamer::packet::{PacketBatch, PacketBatchRecycler};
use solana_streamer::streamer;
use solana_streamer::streamer::StreamerReceiveStats;

/// Bind to ports and start forwarding shreds
#[allow(clippy::too_many_arguments)]
pub fn start_forwarder_threads(
    src_addr: IpAddr,
    src_port: u16,
    num_threads: Option<usize>,
    exit: Arc<AtomicBool>,
) -> Vec<JoinHandle<()>> {
    let num_threads = num_threads
        .unwrap_or_else(|| usize::from(std::thread::available_parallelism().unwrap()).max(4));

    // spawn a thread for each listen socket. linux kernel will load balance amongst shared sockets
    solana_net_utils::multi_bind_in_range(src_addr, (src_port, src_port + 1), num_threads)
        .unwrap_or_else(|_| {
            panic!("Failed to bind listener sockets. Check that port {src_port} is not in use.")
        })
        .1
        .into_iter()
        .enumerate()
        .flat_map(|(thread_id, incoming_shred_socket)| {
            let (packet_sender, packet_receiver) = crossbeam_channel::unbounded();
            let stats = Arc::new(StreamerReceiveStats::new("shredstream_proxy-listen_thread"));
            let listen_thread = streamer::receiver(
                format!("ssListen{thread_id}"),
                Arc::new(incoming_shred_socket),
                exit.clone(),
                packet_sender,
                PacketBatchRecycler::default(),
                stats.clone(),
                Duration::default(), // do not coalesce since batching consumes more cpu cycles and adds latency.
                false,
                None,
                false,
            );

            let send_thread = Builder::new()
                .name(format!("ssPxyTx_{thread_id}"))
                .spawn(move || {
                    loop {
                        crossbeam_channel::select! {
                            // forward packets
                            recv(packet_receiver) -> maybe_packet_batch => {
                               save_packet_to_file(maybe_packet_batch);
                            }
                        }
                    }

                })
                .unwrap();

            [listen_thread, send_thread]
        })
        .collect::<Vec<JoinHandle<()>>>()
}

fn save_packet_to_file(maybe_packet: Result<PacketBatch, RecvError>) {
    let batch = maybe_packet.unwrap();
    let packets = batch.iter().filter_map(|pkt| {
        let data = pkt.data(..)?;
        Some(data)
    }).collect::<Vec<&[u8]>>();

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("udp_packets.bin").unwrap();

    for packet in packets.clone() {
        let packet_len = packet.len() as u32;

        file.write_all(&packet_len.to_le_bytes()).unwrap();
        file.write_all(packet).unwrap();
        file.flush().unwrap();
        println!("len: {packet_len}");
    }
    file.write_all(b"Batch end").unwrap();
    println!("Batch len: {:?}", packets.len());
}


fn read_packets_from_file() -> Vec<Vec<Vec<u8>>> {
    let mut file = File::open("udp_packets.bin").unwrap();

    // Буфер для чтения длины пакета (4 байта для u32)
    let mut length_buf = [0u8; 4];

    let mut batch = vec![];
    let mut results = vec![];
    loop {
        // Читаем длину пакета
        if file.read_exact(&mut length_buf).is_err() {
            break; // Выходим из цикла, если данных больше нет
        }
        let packet_len = u32::from_le_bytes(length_buf) as usize;

        // Буфер для данных пакета
        let mut packet_buf = vec![0u8; packet_len];

        // Читаем пакет
        if file.read_exact(&mut packet_buf).is_err() {
            break;
        }

        // Обрабатываем пакет (например, выводим его содержимое)
        println!("{:?}", String::from_utf8(packet_buf.clone()).unwrap());
        batch.push(packet_buf);

        let mut batch_symbol = vec![0; 9];
        if file.read_exact(&mut batch_symbol).is_ok() {
            if let Ok(str) = String::from_utf8(batch_symbol.to_vec()) {
                if str.to_string().contains("Batch end") {
                    results.push(batch.clone());
                    batch.clear();
                }
            }
        }

    }
    results
}


fn main() -> std::io::Result<()> {
    let handles = start_forwarder_threads(
        IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
        20000,
        Some(3),
        Arc::new(AtomicBool::default())
    );

    for h in handles {
        h.join().unwrap();
    }
    println!("{:?}", read_packets_from_file());

    Ok(())
}
