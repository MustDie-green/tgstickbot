use teloxide::{dispatching::dialogue::{InMemStorage}, prelude::*, types::{KeyboardButton, KeyboardMarkup}};
use std::vec;
use std::path::Path;
use tokio::fs;
use image::{DynamicImage, GenericImageView, ImageFormat};
use teloxide::net::Download;
use futures::StreamExt;
use teloxide::utils::command::BotCommands;

type MyDialogue = Dialogue<State, InMemStorage<State>>;
type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
use teloxide::types::StickerFormat;
use teloxide::types::InputSticker;
use teloxide::types::InputFile;
use dotenv::dotenv;
use bitranslit::{transliterate, Language};
use rusqlite::{params, Connection, Result as SqlResult};

#[derive(Clone, Default)]
pub enum State {
    #[default]
    Start,
    AwaitingAction {
        file_id: String,
        is_sticker: bool,
    },
    GetPackName {
        file_id: String,
        is_sticker: bool,
    },
    AddingToPack {
        file_id: String,
        is_sticker: bool,
    },
}

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Доступные команды:")]
enum Command {
    #[command(description = "показать это сообщение")]
    Help,
    #[command(description = "начать сначала")]
    Start,
}

#[tokio::main]
async fn main() {
    dotenv().ok(); 
    pretty_env_logger::init();
    log::info!("Starting dialogue bot...");

    let bot = Bot::from_env();

    Dispatcher::builder(
        bot,
        Update::filter_message()
            .enter_dialogue::<Message, InMemStorage<State>, State>()
            .branch(
                dptree::entry()
                    .filter_command::<Command>()
                    .endpoint(handle_command)
            )
            .branch(
                dptree::entry()
                    .filter(|msg: Message| msg.sticker().is_some() || msg.photo().is_some())
                    .endpoint(media_received)
            )
            .branch(dptree::case![State::AwaitingAction { file_id, is_sticker }]
                .endpoint(receive_action))
            .branch(dptree::case![State::GetPackName { file_id, is_sticker }]
                .endpoint(receive_pack_name_and_create_pack))
            .branch(dptree::case![State::AddingToPack { file_id, is_sticker }]
                .endpoint(add_sticker_to_pack)),
    )
    .dependencies(dptree::deps![InMemStorage::<State>::new()])
    .enable_ctrlc_handler()
    .build()
    .dispatch()
    .await;
}

async fn handle_command(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    cmd: Command,
) -> HandlerResult {
    match cmd {
        Command::Help => {
            let help_text = "🤖 Привет! Я бот для создания стикерпаков.\n\n\
                           Что я умею:\n\
                           • Создавать новые стикерпаки\n\
                           • Добавлять стикеры в стикерпаки, созданные через этого бота\n\
                           • Конвертировать изображения в стикеры\n\
                           • Работать с PNG и JPG форматами\n\n\
                           Как использовать:\n\
                           1. Отправьте мне стикер или изображение\n\
                           2. Выберите создать новый стикерпак или добавить в существующий (созданный через этого бота)\n\
                           3. Следуйте инструкциям\n\n\
                           Команды:\n\
                           /help - показать это сообщение\n\
                           /start - начать сначала";
            
            bot.send_message(msg.chat.id, help_text).await?;
            dialogue.update(State::Start).await?;
        }
        Command::Start => {
            bot.send_message(msg.chat.id, "👋 Привет! Отправь мне стикер или картинку, и я помогу создать новый стикерпак или добавить их в существующий (если он был создан через этого бота).").await?;
            dialogue.update(State::Start).await?;
        }
    }
    Ok(())
}

async fn media_received(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
) -> HandlerResult {
    let user_id = msg.chat.id.0;
    let chat_id = ChatId(msg.chat.id.0);
    
    let (file_id, is_sticker) = if let Some(sticker) = msg.sticker() {
        (sticker.file.id.clone(), true)
    } else if let Some(photos) = msg.photo() {
        (photos.last().unwrap().file.id.clone(), false)
    } else {
        return Ok(());
    };

    let conn = Connection::open("stickers.db").expect("Failed to open SQLite database");
    initialize_db(&conn).expect("Failed to initialize database");

    let pack_list = get_user_sticker_packs(&conn, user_id)?;
    if !pack_list.is_empty() {
        let buttons = vec![
            vec![KeyboardButton::new("Добавить в существующий")],
            vec![KeyboardButton::new("Создать новый")],
        ];
        let keyboard = KeyboardMarkup::new(buttons).resize_keyboard();

        bot.send_message(chat_id, "Хотите добавить стикер в существующий стикерпак (созданный через этого бота) или создать новый?")
            .reply_markup(keyboard)
            .await?;
        
        dialogue.update(State::AwaitingAction { file_id, is_sticker }).await?;
    } else {
        bot.send_message(chat_id, "У вас пока нет стикерпаков, созданных через этого бота. Пожалуйста, введите название для нового стикерпака.").await?;
        dialogue.update(State::GetPackName { file_id, is_sticker }).await?;
    }

    Ok(())
}

async fn receive_action(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    (file_id, is_sticker): (String, bool),
) -> HandlerResult {
    let user_id = msg.chat.id.0;
    let conn = Connection::open("stickers.db").expect("Failed to open SQLite database");
    initialize_db(&conn).expect("Failed to initialize database");
    
    match msg.text().map(ToOwned::to_owned) {
        Some(source) => {
            match source.as_str() {
                "Добавить в существующий" | "Добавить в другой" => {
                    let pack_list = get_user_sticker_packs(&conn, user_id)?;
                    if pack_list.is_empty() {
                        bot.send_message(msg.chat.id, "У вас пока нет стикерпаков. Пожалуйста, введите название для нового стикерпака:").await?;
                        dialogue.update(State::GetPackName { file_id, is_sticker }).await?;
                        return Ok(());
                    }

                    let buttons: Vec<Vec<KeyboardButton>> = pack_list
                        .iter()
                        .map(|pack_name| vec![KeyboardButton::new(pack_name.clone())])
                        .collect();
                    let keyboard = KeyboardMarkup::new(buttons).resize_keyboard();
                    
                    bot.send_message(msg.chat.id, "Выберите стикерпак, в который хотите добавить стикер:")
                        .reply_markup(keyboard)
                        .await?;

                    dialogue.update(State::AddingToPack { file_id, is_sticker }).await?;
                }
                "Создать новый" => {
                    bot.send_message(msg.chat.id, "Введите название для нового стикерпака:").await?;
                    dialogue.update(State::GetPackName { file_id, is_sticker }).await?;
                }
                _ => {
                    bot.send_message(msg.chat.id, "Пожалуйста, используйте кнопки для выбора действия").await?;
                    dialogue.exit().await?
                }
            }
        }
        None => {
            bot.send_message(msg.chat.id, "Пожалуйста, используйте кнопки для выбора действия")
                .await?;
            dialogue.exit().await?
        }
    }
    Ok(())
}

async fn process_image(bot: &Bot, file_id: &str, user_id: i64) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let file = bot.get_file(file_id).await?;
    let temp_dir = Path::new("temp");
    if !temp_dir.exists() {
        fs::create_dir(temp_dir).await?;
    }

    let mut file_content = Vec::new();
    let mut stream = bot.download_file_stream(&file.path);
    while let Some(chunk) = stream.next().await {
        file_content.extend_from_slice(&chunk?);
    }

    let format = image::guess_format(&file_content)?;
    let extension = match format {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Png => "png",
        ImageFormat::WebP => "webp",
        _ => "png",
    };
    
    let input_path = temp_dir.join(format!("input.{}", extension));
    let output_path = temp_dir.join("output.png");
    
    fs::write(&input_path, &file_content).await?;
    let img = image::load_from_memory(&file_content)?;
    let processed = process_image_for_sticker(img)?;
    processed.save_with_format(&output_path, ImageFormat::Png)?;
    
    let input_file = InputFile::file(&output_path);
    let uploaded = bot.upload_sticker_file(UserId(user_id as u64), input_file, StickerFormat::Static).await?;
    
    fs::remove_file(&input_path).await?;
    fs::remove_file(&output_path).await?;
    
    Ok(uploaded.id)
}

fn process_image_for_sticker(img: DynamicImage) -> Result<DynamicImage, Box<dyn std::error::Error + Send + Sync>> {
    let border_size = 50;
    let bordered_width = img.width() + 2 * border_size;
    let bordered_height = img.height() + 2 * border_size;
    let mut bordered_image = DynamicImage::new_rgba8(bordered_width, bordered_height);

    image::imageops::overlay(&mut bordered_image, &img, border_size as u32, border_size as u32);

    let (width, height) = (bordered_image.width(), bordered_image.height());
    let aspect_ratio = width as f32 / height as f32;

    let (new_width, new_height) = if width > height {
        (512, (512.0 / aspect_ratio).round() as u32)
    } else {
        ((512.0 * aspect_ratio).round() as u32, 512)
    };

    let resized = bordered_image.resize_exact(new_width, new_height, image::imageops::FilterType::Lanczos3);

    Ok(resized)
}

fn check_sticker_pack_exists(conn: &Connection, user_id: i64, id_pack_name: &str) -> SqlResult<bool> {
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM sticker_packs WHERE user_id = ?1 AND id_pack_name = ?2")?;
    let count: i64 = stmt.query_row(params![user_id, id_pack_name], |row| row.get(0))?;
    Ok(count > 0)
}

async fn receive_pack_name_and_create_pack(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    (file_id, is_sticker): (String, bool),
) -> HandlerResult {
    let conn = Connection::open("stickers.db").expect("Failed to open SQLite database");
    initialize_db(&conn).expect("Failed to initialize database");

    let user_id: i64 = msg.chat.id.0;
    let user_id_2 = UserId(user_id as u64);
    let chat_id = ChatId(msg.chat.id.0);

    if let Some(pack_name) = msg.text() {
        let username = if let Some(user) = &msg.from() {
            user.username.clone().unwrap_or_else(|| format!("user{}", user_id))
        } else {
            format!("user{}", user_id)
        };
        let id_pack_name = process_string(&format!("{pack_name}_{username}_by_flex_stickerpack_bot"));

        if check_sticker_pack_exists(&conn, user_id, &id_pack_name).expect("Failed to check sticker pack") {
            bot.send_message(chat_id, "У вас уже есть стикерпак с таким именем. Пожалуйста, выберите другое имя.").await?;
            return Ok(());
        }

        save_sticker_pack(&conn, user_id, &pack_name, &id_pack_name).expect("Failed to save sticker pack");

        let processed_file_id = if !is_sticker {
            match process_image(&bot, &file_id, user_id).await {
                Ok(new_file_id) => new_file_id,
                Err(e) => {
                    bot.send_message(chat_id, format!("Ошибка при обработке изображения: {}", e)).await?;
                    return Ok(());
                }
            }
        } else {
            file_id.clone()
        };

        let sticker = vec![InputSticker {
            sticker: InputFile::file_id(processed_file_id),
            emoji_list: vec!["💬".to_string()],
            mask_position: None,
            keywords: vec!["quote".to_string()],
        }];

        bot.send_message(chat_id, format!("Создаем новый стикерпак: {id_pack_name}, название: {pack_name}")).await?;

        match bot.create_new_sticker_set(
            user_id_2,
            id_pack_name.clone(),
            pack_name,
            sticker,
            StickerFormat::Static,
        ).await {
            Ok(_) => {
                bot.send_message(chat_id, format!("Держи свой стикерпак t.me/addstickers/{id_pack_name}")).await?;
            }
            Err(err) => {
                conn.execute(
                    "DELETE FROM sticker_packs WHERE user_id = ?1 AND id_pack_name = ?2",
                    params![user_id, id_pack_name],
                ).expect("Failed to delete sticker pack record");
                
                bot.send_message(chat_id, format!("Не удалось создать стикерпак: {err}")).await?;
            }
        }

        dialogue.exit().await?;
    }

    Ok(())
}

async fn add_sticker_to_pack(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    (file_id, is_sticker): (String, bool),
) -> HandlerResult {
    let user_id = msg.chat.id.0;
    let user_id_2 = UserId(user_id as u64);
    let chat_id = ChatId(msg.chat.id.0);

    if let Some(pack_name) = msg.text() {
        let processed_file_id = if !is_sticker {
            match process_image(&bot, &file_id, user_id).await {
                Ok(new_file_id) => new_file_id,
                Err(e) => {
                    bot.send_message(chat_id, format!("Ошибка при обработке изображения: {}", e)).await?;
                    return Ok(());
                }
            }
        } else {
            file_id.clone()
        };

        let sticker = InputSticker {
            sticker: InputFile::file_id(processed_file_id),
            emoji_list: vec!["💬".to_string()],
            mask_position: None,
            keywords: vec!["quote".to_string()],
        };

        match bot.add_sticker_to_set(user_id_2, pack_name, sticker).await {
            Ok(_) => {
                bot.send_message(chat_id, format!("Стикер добавлен в стикерпак t.me/addstickers/{pack_name}.")).await?;
                dialogue.exit().await?;
            }
            Err(err) => {
                if err.to_string().contains("STICKERSET_INVALID") {
                    let conn = Connection::open("stickers.db").expect("Failed to open SQLite database");
                    conn.execute(
                        "DELETE FROM sticker_packs WHERE user_id = ?1 AND id_pack_name = ?2",
                        params![user_id, pack_name],
                    ).expect("Failed to delete sticker pack record");

                    let pack_list = get_user_sticker_packs(&conn, user_id)?;
                    if !pack_list.is_empty() {
                        let buttons = vec![
                            vec![KeyboardButton::new("Добавить в другой")],
                            vec![KeyboardButton::new("Создать новый")],
                        ];
                        let keyboard = KeyboardMarkup::new(buttons).resize_keyboard();

                        bot.send_message(chat_id, "Этот стикерпак больше не существует. Хотите добавить стикер в другой стикерпак или создать новый?")
                            .reply_markup(keyboard)
                            .await?;
                        
                        dialogue.update(State::AwaitingAction { file_id, is_sticker }).await?;
                    } else {
                        bot.send_message(chat_id, "Этот стикерпак больше не существует. Пожалуйста, введите название для нового стикерпака:").await?;
                        dialogue.update(State::GetPackName { file_id, is_sticker }).await?;
                    }
                } else {
                    bot.send_message(chat_id, format!("Не удалось добавить стикер в стикерпак: {err}")).await?;
                    dialogue.exit().await?;
                }
            }
        }
    } else {
        bot.send_message(msg.chat.id, "Пожалуйста, используйте кнопки для выбора стикерпака")
            .await?;
        dialogue.exit().await?
    }
    Ok(())
}

fn process_string(input: &str) -> String {
    transliterate(&input.replace(" ", "_"), Language::Russian, false)
}

fn initialize_db(conn: &Connection) -> SqlResult<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sticker_packs (
            user_id INTEGER,
            pack_name TEXT,
            id_pack_name TEXT,
            PRIMARY KEY (user_id, id_pack_name)
        )",
        [],
    )?;
    Ok(())
}

fn save_sticker_pack(conn: &Connection, user_id: i64, pack_name: &str, id_pack_name: &str) -> SqlResult<()> {
    conn.execute(
        "INSERT INTO sticker_packs (user_id, pack_name, id_pack_name) VALUES (?1, ?2, ?3)",
        params![user_id, pack_name, id_pack_name],
    )?;
    Ok(())
}

fn get_user_sticker_packs(conn: &Connection, user_id: i64) -> SqlResult<Vec<String>> {
    let mut stmt = conn.prepare("SELECT id_pack_name FROM sticker_packs WHERE user_id = ?1")?;
    let packs_iter = stmt.query_map([user_id], |row| row.get(0))?;

    let mut packs = Vec::new();
    for pack in packs_iter {
        packs.push(pack?);
    }
    Ok(packs)
}
