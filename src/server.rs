use std::error::Error;
use std::fmt;
use std::time::Duration;
use tokio::io::{ReadHalf, WriteHalf, BufReader, BufWriter, AsyncBufReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use std::net::SocketAddr;

struct Comms<T: std::clone::Clone> {
  sender: broadcast::Sender<T>,
  receiver: broadcast::Receiver<T>,
}

impl<T: std::clone::Clone> Comms<T> {
  fn new(capacity: usize) -> Comms<T> {
    let (sender, receiver) = broadcast::channel(capacity);
    Comms { sender, receiver }
  }
  fn clone(&self) -> Comms<T> {
    Comms {
      sender: self.sender.clone(),
      receiver: self.sender.subscribe(),
    }
  }
}

#[derive(Clone, Debug)]
struct Message {
  address: SocketAddr,
  payload: String,
}

struct ClientInfo {
  server_addr: SocketAddr,
  socket: TcpStream,
  addr: SocketAddr,
}

impl ClientInfo {
  fn new(server_addr: SocketAddr, socket: TcpStream, addr: SocketAddr) -> Self {
    ClientInfo { server_addr, socket, addr }
  }
}

impl Message {
  fn new(addr: SocketAddr, payload: String) -> Message {
    Message { address: addr, payload }
  }

  fn as_bytes(&self) -> Vec<u8> {
    let mut result = self.address.to_string().into_bytes();
    result.extend_from_slice(b": ");
    result.extend_from_slice(self.payload.as_bytes());
    result
  }
}

impl fmt::Display for Message {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
      write!(f, "{}: {}", self.address, self.payload)
  }
}

pub async fn run(server_addr: &'static str, port: u16) -> Result<(), Box<dyn Error>> {
  let listener = TcpListener::bind(format!("{}:{}", server_addr, port)).await?;
  let comms = Comms::new(100);

  loop {
    let (socket, addr) = listener.accept().await?;
    let client_info = ClientInfo::new(listener.local_addr()?, socket, addr);
    println!("Client {addr} connected to the server");

    let comms_clone = comms.clone();
    tokio::spawn(async move {
      match handle_client(client_info, comms_clone).await {
        Ok(()) => {
          println!("{addr} disconnected from the server");
        },
        Err(e) => println!("Error: {:?}", e),
      }
    });
  }
}

async fn handle_client(client_info: ClientInfo, global_comms: Comms<Message>) -> Result<(), Box<dyn Error>> {
  let (read_half, write_half) = tokio::io::split(client_info.socket);
  let anouncer = global_comms.sender.clone();

  // Spawns a task that solely listens for messages from the client.
  let listener = tokio::spawn(async move {
    let buf_reader = BufReader::new(read_half);
    run_client_listener(client_info.addr, buf_reader, global_comms.sender).await
  });
  // Spawns a task that sends messages back to the client.
  let writer = tokio::spawn(async move {
    let buf_writer = BufWriter::new(write_half);
    run_client_writer(client_info.addr, buf_writer, global_comms.receiver).await 
  });

  let mut err = Result::Ok(());
  loop {
    if listener.is_finished() {
      let listener_res = listener.await?;
      err = listener_res;
      let abort_writer = writer.abort_handle();
      // Give the writer a chance to finish.
      match tokio::time::timeout(Duration::from_millis(1000), writer).await {
        Ok(v) => {
          let writer_res = v?;
          if writer_res.is_err() && ! err.is_err() {
            err = writer_res;
          }
        },
        Err(_) => { // Writer timed out, so ignore it an kill it.
          abort_writer.abort();
        },
      };
      break;
    }

    // Report the error of whichever task finishes first and kill the other one.
    if writer.is_finished() {
      let writer_res = writer.await?;
      err = writer_res;
      let abort_listener = listener.abort_handle();
      // Give the writer a chance to finish.
      match tokio::time::timeout(Duration::from_millis(1000), listener).await {
        Ok(v) => {
          let listener_res = v?;
          if listener_res.is_err() && ! err.is_err() {
            err = listener_res;
          }
        },
        Err(_) => { // Writer timed out, so ignore it an kill it.
          abort_listener.abort();
        },
      };
      break;
    }
    tokio::task::yield_now().await;
  };

  println!("{:?}", err);

  // Anounce user disconnected. 
  let msg = format!("Client {} disconnected from the server", client_info.addr);
  let _ = anouncer.send(Message { address: client_info.server_addr, payload: msg });
  // Rust is dumb, doesn't allow just returning err.
  match err {
    Ok(()) => Ok(()),
    Err(e) => Err(e),
  }
}

async fn run_client_listener(
  addr: SocketAddr,
  mut reader: BufReader<ReadHalf<TcpStream>>,
  global_comms: broadcast::Sender<Message>) -> Result<(), Box<dyn Error + Send>> {
  loop {
    let mut line = String::new();
    let _bytes_read = match reader.read_line(&mut line).await {
      Ok(0) => {
        return Ok(());
      }
      Ok(bytes_read) => bytes_read,
      Err(e) => return Err(Box::new(e)),
    };

    // Broadcast the user message.
    // If this fails, then we know our sibling has died (since our sibling is always connected to the channel).
    let msg = Message::new(addr.clone(), line);
    match global_comms.send(msg) {
      Ok(_) => (),
      Err(e) => return Err(Box::new(e)),
    }
  }
}

async fn run_client_writer(
  _addr: SocketAddr,
  mut writer: BufWriter<WriteHalf<TcpStream>>,
  mut global_comms: broadcast::Receiver<Message>) -> Result<(), Box<dyn Error + Send>> {
  loop {
    // Listen to the global comms.
    let msg = match global_comms.recv().await {
      Ok(msg) => msg,
      Err(e) => return Err(Box::new(e)),
    };

    // println!("{}", msg);
    // if msg.address == addr {
    //   continue;
    // }

    // Send the message to the user.
    match writer.write_all(&msg.as_bytes()).await {
      Ok(()) => (),
      Err(e) => return Err(Box::new(e)),
    }
    match writer.flush().await {
        Ok(_) => (),
        Err(e) => return Err(Box::new(e)),
    }
  }
}